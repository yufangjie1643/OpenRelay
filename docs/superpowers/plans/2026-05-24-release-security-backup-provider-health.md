# Release, Security, Backup, Provider Health Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add Windows release/update affordances, startup controls, secret protection, security audits, config backup/restore, provider templates, and provider health checks to OpenRelay.

**Architecture:** Keep `src/server.rs` as the route wiring layer and move new behavior into focused Rust modules: `src/secrets.rs`, `src/backups.rs`, `src/release.rs`, and `src/health.rs`. The admin UI stays static in `public/index.html` and calls authenticated JSON APIs.

**Tech Stack:** Rust 2021, Axum, Reqwest, Serde, Windows DPAPI via `windows-sys`, PowerShell helper scripts, static HTML/CSS/JS, existing `cargo test` integration tests.

---

### Task 1: Backup and Redacted Export Core

**Files:**
- Create: `src/backups.rs`
- Modify: `src/lib.rs`
- Test: `tests/config_io.rs`

- [ ] Write tests for snapshot creation, listing, redacted export, restore, and import validation.
- [ ] Implement timestamped backup IDs, safe path resolution, redaction helpers, and retention-friendly metadata.
- [ ] Run `cargo test --test config_io`.

### Task 2: Secret Protection and Security Audit

**Files:**
- Create: `src/secrets.rs`
- Modify: `Cargo.toml`
- Modify: `src/config.rs`
- Test: `tests/config_io.rs`
- Test: `tests/http_routes.rs`

- [ ] Write tests for default credential audit findings and provider API key protection behavior.
- [ ] Implement protected provider key format `protected:dpapi:<base64>` on Windows and unsupported/no-op behavior elsewhere.
- [ ] Update `load_config` to unprotect provider keys into memory and `save_config` callers to preserve protected-at-rest behavior when requested.
- [ ] Run `cargo test --test config_io --test http_routes`.

### Task 3: Provider Health and Templates

**Files:**
- Create: `src/health.rs`
- Modify: `src/server.rs`
- Test: `tests/http_routes.rs`

- [ ] Add Bailian to provider presets with OpenAI-compatible base URL, environment placeholder, and common models.
- [ ] Write mock-upstream tests for provider health success, unauthorized, rate limit/quota, and network/timeout classifications.
- [ ] Implement provider health checks with latency, status, model count, and safe error messages.
- [ ] Wire `GET /api/providers/health` and `POST /api/providers/health`.
- [ ] Run `cargo test --test http_routes`.

### Task 4: Release, Startup, and Update Status

**Files:**
- Create: `src/release.rs`
- Create: `install.ps1`
- Create: `uninstall.ps1`
- Create: `package-windows.ps1`
- Modify: `src/server.rs`
- Test: `tests/http_routes.rs`
- Test: `tests/repo_layout.rs`

- [ ] Write tests for app status route fields and repository packaging files.
- [ ] Implement version/data/static/startup status response.
- [ ] Implement explicit startup enable/disable using Windows Startup folder shortcuts where supported, with safe unsupported responses elsewhere.
- [ ] Implement update-check route against GitHub releases with graceful network failure behavior.
- [ ] Add PowerShell install/uninstall/package scripts.
- [ ] Run `cargo test --test http_routes --test repo_layout`.

### Task 5: Admin UI Integration

**Files:**
- Modify: `public/index.html`
- Test: `tests/repo_layout.rs`

- [ ] Add provider health controls and per-provider health badges to the model configuration tab.
- [ ] Add release/update/startup/security/backup panels to system settings.
- [ ] Add JS functions for security audit, secret protection, manual backup, backup list, redacted export, import, restore, update check, and startup toggle.
- [ ] Keep text compact, avoid nested cards, and preserve responsive layout.
- [ ] Run `cargo test --test repo_layout`.

### Task 6: Full Verification

**Files:**
- Modify as needed from prior tasks.

- [ ] Run `cargo fmt`.
- [ ] Run `cargo test`.
- [ ] Build release with `cargo build --release` if tests pass.
- [ ] Summarize any platform-limited behavior, especially DPAPI/startup/update behavior outside Windows.
