use crate::{
    model::{clock_valid, now, valid_id, Role},
    store::Database,
};
use anyhow::{bail, ensure, Context, Result};
use argon2::{
    password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString},
    Algorithm, Argon2, Params, Version,
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD as B64, Engine};
use hmac::{Hmac, Mac};
use rand::{rngs::OsRng, RngCore};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

#[derive(Clone, Deserialize, Serialize)]
pub struct User {
    pub password_hash: String,
    pub role: Role,
    pub disabled: bool,
    pub version: u64,
}
#[derive(Clone, Deserialize, Serialize)]
pub struct ApiKey {
    pub id: String,
    pub name: String,
    pub username: String,
    pub hash: String,
}
#[derive(Clone, Deserialize, Serialize)]
pub struct Claims {
    pub sub: String,
    pub iss: String,
    pub aud: String,
    pub iat: i64,
    pub exp: i64,
    pub ver: u64,
}
#[derive(Clone, Serialize)]
pub struct Principal {
    pub username: String,
    pub role: Role,
}
pub fn random(n: usize) -> String {
    let mut bytes = vec![0; n];
    OsRng.fill_bytes(&mut bytes);
    B64.encode(bytes)
}
fn argon() -> Argon2<'static> {
    // Bounded to 4 MiB on the 105 MiB target. Login is also globally throttled.
    Argon2::new(
        Algorithm::Argon2id,
        Version::V0x13,
        Params::new(4096, 3, 1, None).expect("fixed Argon2 parameters"),
    )
}
pub fn user(password: &str, role: Role) -> Result<User> {
    ensure!(
        (12..=128).contains(&password.len()),
        "password must be 12..128 bytes"
    );
    let salt = SaltString::generate(&mut OsRng);
    let password_hash = argon()
        .hash_password(password.as_bytes(), &salt)
        .map_err(|_| anyhow::anyhow!("password hashing failed"))?
        .to_string();
    Ok(User {
        password_hash,
        role,
        disabled: false,
        version: 1,
    })
}
pub fn password_ok(user: &User, password: &str) -> bool {
    !user.disabled
        && password.len() <= 128
        && PasswordHash::new(&user.password_hash)
            .ok()
            .is_some_and(|h| argon().verify_password(password.as_bytes(), &h).is_ok())
}
fn digest(key: &str) -> String {
    B64.encode(Sha256::digest(key.as_bytes()))
}
pub fn issue_key(db: &mut Database, username: &str, name: &str) -> Result<String> {
    ensure!(
        db.users.get(username).is_some_and(|u| !u.disabled),
        "unknown account"
    );
    ensure!(
        db.api_keys.len() < 32 && !name.is_empty() && name.len() <= 80,
        "invalid key name / key limit reached"
    );
    let id = random(9);
    let key = format!("fl_{}_{}", id, random(32));
    db.api_keys.push(ApiKey {
        id,
        name: name.to_owned(),
        username: username.to_owned(),
        hash: digest(&key),
    });
    Ok(key)
}
pub fn issue_jwt(username: &str, user: &User, secret: &[u8], issuer: &str) -> Result<String> {
    ensure!(
        clock_valid(),
        "clock unset; set device time via SSH before logging in"
    );
    let h = B64.encode(br#"{"alg":"HS256","typ":"JWT"}"#);
    let c = Claims {
        sub: username.to_owned(),
        iss: issuer.to_owned(),
        aud: "furbo-api".into(),
        iat: now(),
        exp: now() + 3600,
        ver: user.version,
    };
    let data = format!("{}.{}", h, B64.encode(serde_json::to_vec(&c)?));
    let mut mac = Hmac::<Sha256>::new_from_slice(secret)?;
    mac.update(data.as_bytes());
    Ok(format!(
        "{}.{}",
        data,
        B64.encode(mac.finalize().into_bytes())
    ))
}
pub fn verify(token: &str, db: &Database, secret: &[u8], issuer: &str) -> Result<Principal> {
    ensure!(token.len() <= 2048, "invalid credentials");
    if token.starts_with("fl_") {
        let hash = digest(token);
        for key in &db.api_keys {
            if bool::from(hash.as_bytes().ct_eq(key.hash.as_bytes())) {
                let u = db.users.get(&key.username).context("account removed")?;
                ensure!(!u.disabled, "account disabled");
                return Ok(Principal {
                    username: key.username.clone(),
                    role: u.role,
                });
            }
        }
        bail!("invalid credentials");
    }
    ensure!(clock_valid(), "clock unset");
    let parts: Vec<_> = token.split('.').collect();
    ensure!(parts.len() == 3, "invalid JWT");
    // Require exactly our fixed HS256 header: no algorithm negotiation.
    ensure!(
        parts[0] == B64.encode(br#"{"alg":"HS256","typ":"JWT"}"#),
        "invalid JWT header"
    );
    let data = format!("{}.{}", parts[0], parts[1]);
    let mut mac = Hmac::<Sha256>::new_from_slice(secret)?;
    mac.update(data.as_bytes());
    mac.verify_slice(&B64.decode(parts[2])?)
        .map_err(|_| anyhow::anyhow!("invalid JWT signature"))?;
    let c: Claims = serde_json::from_slice(&B64.decode(parts[1])?)?;
    ensure!(
        c.iss == issuer
            && c.aud == "furbo-api"
            && c.exp > now()
            && c.iat <= now() + 30
            && c.iat <= c.exp
            && c.exp - c.iat <= 3600,
        "expired or invalid JWT"
    );
    let u = db.users.get(&c.sub).context("unknown account")?;
    ensure!(!u.disabled && c.ver == u.version, "revoked JWT");
    Ok(Principal {
        username: c.sub,
        role: u.role,
    })
}
pub fn validate_username(s: &str) -> Result<()> {
    ensure!(valid_id(s) && s.len() <= 40, "invalid username");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn tokens_and_revocation() {
        let mut db = Database::default();
        db.users.insert(
            "admin".into(),
            user("a-long-test-password", Role::Admin).unwrap(),
        );
        let u = db.users["admin"].clone();
        assert!(password_ok(&u, "a-long-test-password"));
        assert!(!password_ok(&u, "incorrect"));
        let secret = b"01234567890123456789012345678901";
        let jwt = issue_jwt("admin", &u, secret, "unit").unwrap();
        assert_eq!(verify(&jwt, &db, secret, "unit").unwrap().role, Role::Admin);
        assert!(verify(&jwt, &db, secret, "other-device").is_err());
        assert!(verify(&(jwt + "x"), &db, secret, "unit").is_err());
        let key = issue_key(&mut db, "admin", "test").unwrap();
        assert!(verify(&key, &db, secret, "unit").is_ok());
        db.users.get_mut("admin").unwrap().disabled = true;
        assert!(verify(&key, &db, secret, "unit").is_err());
    }
}
