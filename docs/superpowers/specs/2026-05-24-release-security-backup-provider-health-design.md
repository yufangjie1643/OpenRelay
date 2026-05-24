# Release, Security, Backup, Provider Health Design

## Objective

Turn OpenRelay from a developer-run local proxy into a daily Windows utility with safer secrets, recoverable configuration, provider templates, visible health checks, and release/update affordances.

## Scope

This iteration adds product-complete surfaces for:

- Windows install/uninstall helper scripts, startup registration, version information, update checks, and changelog links.
- Windows DPAPI protection for provider API keys, plus security diagnostics for default password, default master key, missing `JWT_SECRET`, and plaintext key risk.
- Configuration backups, redacted export, import validation, automatic snapshots before config changes, and rollback to a selected snapshot.
- Provider templates including OpenAI, Gemini, DeepSeek, Anthropic, OpenRouter, SiliconFlow, SiliconFlow China, and Alibaba Cloud Bailian.
- Provider health checks showing status, latency, HTTP status, model count, auth failures, rate-limit/quota signals, timeout/network failures, and unresolved environment variables.

This iteration does not perform silent self-updates of a running executable. Windows executable replacement is risky while the process is active, so the product will check GitHub releases and guide users to the release asset or installer.

## Architecture

Backend changes stay in focused Rust modules instead of expanding `src/server.rs` further:

- `src/secrets.rs` owns secret protection and redacted configuration helpers.
- `src/backups.rs` owns snapshot naming, backup list metadata, export/import payloads, restore, and retention.
- `src/release.rs` owns app version, startup shortcut state, update check response shaping, and helper script paths.
- `src/health.rs` owns provider health classification and request timing.
- `src/server.rs` wires new routes into the existing Axum router and keeps admin authentication behavior consistent.

The static admin UI remains in `public/index.html`, following the existing inline HTML/CSS/JS pattern. New controls live under "系统设置" for release/security/backup, and provider health controls live near the existing provider list.

## Data Flow

Saving config through `/api/config` creates an automatic backup before overwriting `config.json`. Backup entries live under `~/.openrelay/backups/` as timestamped JSON files with metadata derived from the saved config. Exports can be redacted so provider API keys and virtual keys are replaced by masked placeholders.

Provider API keys are encrypted at rest on Windows using DPAPI. The in-memory `AppConfig` still contains usable plaintext so existing proxy routing remains unchanged. On non-Windows platforms, secret protection reports unsupported and keeps current behavior.

Provider health checks use each provider's configured model-list endpoint. They resolve `os.environ/NAME` placeholders, time the request, classify common failure classes, and avoid leaking actual API keys in responses.

Update checks query GitHub release metadata only when the user requests it from the UI. Network failures produce a non-blocking warning. Startup registration is explicit and reversible.

## API Surface

- `GET /api/app/status`: version, data path, static path, startup state, security summary.
- `GET /api/app/update-check`: latest GitHub release information and whether an update appears available.
- `POST /api/app/startup`: enable or disable Windows startup registration.
- `GET /api/security/audit`: detailed security findings.
- `POST /api/security/protect-secrets`: rewrite config with protected provider API keys where supported.
- `GET /api/config/backups`: list config snapshots.
- `POST /api/config/backups`: create a manual snapshot.
- `GET /api/config/backups/:id/export?redacted=true`: export one snapshot or current config.
- `POST /api/config/import`: validate and save imported config.
- `POST /api/config/backups/:id/restore`: restore a snapshot.
- `GET /api/providers/health`: check all configured providers.
- `POST /api/providers/health`: check one provided provider payload without saving it.

Existing `/api/provider-presets`, `/api/test-provider`, and `/api/config/validate` remain supported and are enhanced rather than replaced.

## Error Handling

All new admin APIs require the same Bearer admin token as existing config routes. File operations return JSON errors with safe messages. Backup restore/import validates JSON into `AppConfig` before writing. Health checks distinguish invalid URL, missing key, unresolved environment variable, auth failure, quota/rate-limit, timeout, network failure, and upstream HTTP errors.

## Testing

Rust integration tests cover:

- Provider presets include Alibaba Cloud Bailian.
- Security audit flags default password, default master key, missing `JWT_SECRET`, and plaintext keys.
- Secret protection is no-op with a clear unsupported status on non-Windows and round-trips on Windows.
- Backup creation, redacted export, import, and restore preserve config validity.
- Config save creates an automatic snapshot before overwriting.
- Provider health classification works against local mock upstreams for success, unauthorized, rate-limited, and timeout/error paths.
- App status exposes version and startup fields without requiring network access.

Frontend layout tests check that the admin UI exposes update, startup, security audit, backup/restore, provider health, and Bailian template controls.
