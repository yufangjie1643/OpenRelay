# Repository Guidelines

## Project Structure & Module Organization
This repository contains OpenRelay, a Windows-oriented personal unified LLM API gateway plus a small admin UI.

- `web/server.js` hosts the Express app, manages config migration, writes compatibility YAML, tracks usage, and serves the unified OpenAI-compatible proxy on port `18783`.
- `web/public/index.html` and `web/public/login.html` contain the browser UI, inline styles, and client-side scripts.
- `web/package.json` defines the Node entry point and dependencies.
- Root scripts (`setup.ps1`, `start.bat`, `start-web.bat`, `start-all.bat`, `start-tray.vbs`) handle local setup and Windows startup flows.
- Runtime files such as `config.json`, `openrelay-config.yaml`, `usage.jsonl`, and `conversations/` are local/generated and should not be committed.

## Build, Test, and Development Commands
Run commands from the repository root unless noted.

- `.\setup.ps1` installs Node.js dependencies for the admin UI / proxy.
- `cd web; npm install` installs the admin UI dependencies.
- `cd web; npm run dev` or `npm start` starts the Express admin UI at `http://localhost:18783`.
- `.\start.bat` starts the OpenRelay Web UI / API at `http://localhost:18783`.
- `.\start-all.bat` opens the OpenRelay Web UI / API in a Windows command window.

## Coding Style & Naming Conventions
Use the existing CommonJS Node style in `web/server.js`: `require(...)`, `const`/`let`, two-space indentation, semicolons, and small helper functions with camelCase names. Keep browser code consistent with the current inline HTML/CSS pattern unless you are intentionally introducing a build step. Prefer clear IDs and function names that match UI actions, such as `saveConfig`, `switchTab`, or `openProviderModal`.

## Testing Guidelines
There is no committed automated test suite yet. For now, perform smoke tests after changes:

- Start the UI with `cd web; npm run dev`.
- Confirm login, model configuration save, YAML generation, usage pages, `/v1/models`, and OpenAI-compatible proxy behavior.
- If adding tests, place them in a committed `tests/` directory or use a clear `*.test.js` naming pattern. Do not rely on ignored local scratch files such as `test_ua.py`.

## Commit & Pull Request Guidelines
The current history is minimal and uses descriptive summaries, for example `Initial commit: ...`. Keep future commit subjects short, specific, and sentence case or imperative. Pull requests should describe the change, list manual verification steps, mention config or migration impacts, and include screenshots for UI changes.

## Security & Configuration Tips
Never commit real API keys, `config.json`, generated YAML, usage logs, or conversation records. Use `config.example.json` as the template for shareable configuration. Change the default admin password and provide `JWT_SECRET` in the environment for any shared or persistent deployment.
