# Timezone data

Public-domain IANA tzdb, compiled into TZif and deduplicated by content. The index maps timezone names and aliases to byte ranges in zones.bin.

Source: [IANA timezone releases](https://www.iana.org/time-zones/releases). The current version is recorded in VERSION and the archive digest in SOURCE.json.

Update deliberately with:

```sh
python3 scripts/update-tzdb.py 2026f
cargo test timezone
```

The data is embedded in the static service. It does not depend on the factory firmware's old timezone files.
