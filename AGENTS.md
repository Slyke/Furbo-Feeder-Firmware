# Repository instructions

- Keep device-specific credentials, endpoints, identifiers, keys, raw firmware backups, logs, and user media in ignored local files. Never commit them or include them in test fixtures.
- Runtime configuration is JSON. The public example must contain placeholders.
- Use the simulator for routine tests. Real camera/microphone tests capture private material; motor tests need explicit clearance.
- Preserve pinned SSH and a recovery path when changing networking. Stage a timed rollback before testing AP transitions remotely.
- Wi-Fi trials require explicit confirmation within five minutes and are discarded after an unconfirmed reboot.
- Do not erase MTD or automatically format a partition. Retain factory services that provide the ISP, encoder, and audio interfaces.
- Keep all MQTT traffic within the configured root. Commands use cmnd, acknowledgments stat, and telemetry tele.
- Validate with cargo fmt, clippy, tests, the ARMv7 musl build, and frontend syntax/browser checks.
- On Windows, run Linux commands through wsl.exe -d Ubuntu -- bash -lc. Use binary SSH streams for file transfers.
