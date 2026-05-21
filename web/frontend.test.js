const assert = require('assert');
const fs = require('fs');
const path = require('path');

const root = __dirname;
const indexHtml = fs.readFileSync(path.join(root, 'public', 'index.html'), 'utf8');
const loginHtml = fs.readFileSync(path.join(root, 'public', 'login.html'), 'utf8');
const serverJs = fs.readFileSync(path.join(root, 'server.js'), 'utf8');
const repoRoot = path.join(root, '..');
const trayLauncherPath = path.join(root, '..', 'start-tray.c');
const trayLauncherC = fs.existsSync(trayLauncherPath) ? fs.readFileSync(trayLauncherPath, 'utf8') : '';
const trayShim = fs.readFileSync(path.join(root, '..', 'start-tray.vbs'), 'utf8');
const trayIconPath = path.join(root, '..', 'assets', 'openrelay.ico');
const trayResourcePath = path.join(root, '..', 'start-tray.rc');
const trayResource = fs.existsSync(trayResourcePath) ? fs.readFileSync(trayResourcePath, 'utf8') : '';
const webTrayIconPath = path.join(root, 'icon.ico');
const startBat = fs.readFileSync(path.join(root, '..', 'start.bat'), 'utf8');
const startRustBat = fs.readFileSync(path.join(root, '..', 'start-rust.bat'), 'utf8');
const startAllBat = fs.readFileSync(path.join(root, '..', 'start-all.bat'), 'utf8');
const setupPs1 = fs.readFileSync(path.join(root, '..', 'setup.ps1'), 'utf8');
const rustCargoToml = fs.readFileSync(path.join(repoRoot, 'rust-backend', 'Cargo.toml'), 'utf8');
const rustMain = fs.readFileSync(path.join(repoRoot, 'rust-backend', 'src', 'main.rs'), 'utf8');
const rustTrayPath = path.join(repoRoot, 'rust-backend', 'src', 'tray.rs');
const rustTray = fs.existsSync(rustTrayPath) ? fs.readFileSync(rustTrayPath, 'utf8') : '';

function hasCssRule(selector) {
  const escaped = selector.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
  return new RegExp(`${escaped}\\s*\\{`).test(indexHtml);
}

assert(
  !/https:\/\/cdnjs\.cloudflare\.com\/ajax\/libs\/js-yaml/.test(indexHtml),
  'index.html should not depend on CDN js-yaml; local admin UI must work offline'
);

assert(
  /\/vendor\/js-yaml/.test(indexHtml) && /\/vendor\/js-yaml/.test(serverJs),
  'js-yaml browser bundle should be served from local node_modules via /vendor/js-yaml'
);

assert(/process\.env\.PORT/.test(serverJs), 'server should support PORT override for smoke tests and port conflicts');
assert(/const PORT = Number\(process\.env\.PORT\) \|\| 18783;/.test(serverJs), 'server should default to port 18783');
assert(/process\.argv\.includes\('--tray'\)/.test(serverJs), 'system tray should be opt-in so normal web startup cannot crash on tray helper output');
assert(!/localhost:4000|port `4000`|--port', '4000'|--port 4000/.test(serverJs), 'server must not depend on a separate service on port 4000');
assert(!/litellm\.exe|restartLiteLLMProxy/.test(serverJs), 'server must not start or restart a separate litellm.exe process');
assert(rustTray, 'rust-backend/src/tray.rs should integrate the Windows tray into openrelay.exe');
assert(/openrelay::tray::run/.test(rustMain), 'Windows openrelay.exe should start the integrated Rust tray by default');
assert(/OPENRELAY_NO_TRAY/.test(rustMain), 'Rust binary should keep an environment escape hatch for console-only server runs');
assert(/windows-sys/.test(rustCargoToml), 'Rust tray integration should use native Windows APIs from the Rust binary');
assert(/Shell_NotifyIconW/.test(rustTray), 'Rust tray integration should own the tray icon inside openrelay.exe');
assert(/打开管理面板/.test(rustTray) && /重启服务/.test(rustTray) && /退出 OpenRelay/.test(rustTray), 'Rust tray right-click menu should use Chinese labels');
assert(/BackendCommand::Restart/.test(rustTray), 'Rust tray menu should expose a backend restart action');
assert(/ShellExecuteW/.test(rustTray), 'Rust tray open action should launch the local admin panel');
assert(!trayLauncherC || !/server\.js|node\.exe/.test(trayLauncherC), 'legacy C tray launcher must not start the old Node server');
assert(/rust-backend\\target\\release\\openrelay\.exe/.test(trayShim), 'start-tray.vbs should launch the integrated Rust release executable');
assert(!/start-tray\.exe/.test(trayShim), 'start-tray.vbs should no longer delegate to the compiled C launcher');
assert(!/\.venv\\Scripts\\node\.exe/.test(trayShim), 'start-tray.vbs must not hard-require node.exe inside the Python virtual environment');
assert(/rust-backend\\target\\release\\openrelay\.exe/.test(startRustBat), 'start-rust.bat should launch the integrated Rust release executable');
assert(fs.existsSync(trayIconPath) && fs.statSync(trayIconPath).size > 0, 'OpenRelay tray icon should be stored at assets/openrelay.ico');
assert(fs.existsSync(webTrayIconPath) && fs.readFileSync(webTrayIconPath).equals(fs.readFileSync(trayIconPath)), 'web/icon.ico should match the OpenRelay tray launcher icon');
assert(!trayResource || /IDI_APP_ICON\s+ICON\s+"assets\/openrelay\.ico"/.test(trayResource), 'tray resource should embed assets/openrelay.ico when present');
assert(!/localhost:4000/.test(indexHtml), 'admin UI should advertise only the unified 18783 endpoint');
assert(!/4000|litellm\.exe|\.venv/.test(startBat + startAllBat + setupPs1), 'startup/setup scripts must not start a separate 4000 service');
assert(hasCssRule('.btn-secondary'), 'btn-secondary is used by the UI but has no CSS rule');
assert(/--success\s*:/.test(indexHtml), 'CSS variable --success is referenced but not defined');
assert(/function escapeAttr\(/.test(indexHtml), 'attribute escaping helper is required for dynamic HTML attributes');
assert(/:root\[data-theme="light"\]/.test(indexHtml), 'admin UI should define a light color theme');
assert(/:root\[data-theme="dark"\]/.test(indexHtml), 'admin UI should define an explicit dark color theme');
assert(/id="themeModeControl"/.test(indexHtml), 'settings page should expose a theme mode segmented control');
assert(/data-theme-mode="system"/.test(indexHtml) && /data-theme-mode="light"/.test(indexHtml) && /data-theme-mode="dark"/.test(indexHtml), 'theme control should offer system, light, and dark modes');
assert(/localStorage\.setItem\(THEME_STORAGE_KEY/.test(indexHtml), 'theme mode should be persisted locally');
assert(/matchMedia\('\(prefers-color-scheme: dark\)'/.test(indexHtml), 'system theme mode should follow prefers-color-scheme');
assert(/function applyThemeMode\(/.test(indexHtml) && /function setThemeMode\(/.test(indexHtml), 'theme mode should have apply and update handlers');
assert(/供应商自带 Web Search/.test(indexHtml), 'provider configuration should explain native web search pass-through support');
assert(!/value="requests" checked> 数量/.test(indexHtml), 'usage log should show raw recent rows, not merged request-count rows');
assert(!/requests:\s*\{\s*label:\s*'数量'/.test(indexHtml), 'usage log columns should not render merged request counts');
assert(/pg\.limited/.test(indexHtml) && /hiddenTotal/.test(indexHtml), 'usage pagination should disclose hidden old rows');
assert(/已归档/.test(indexHtml), 'usage pagination should describe old rows as archived');
assert(/model:\s*\{\s*label:\s*'模型',\s*render:\s*e\s*=>\s*escapeHtml/.test(indexHtml), 'usage log model values must be HTML-escaped');
assert(/key:\s*\{\s*label:\s*'密钥',\s*render:\s*e\s*=>\s*escapeHtml/.test(indexHtml), 'usage log key values must be HTML-escaped');
assert(/\.conv-box\{[^}]*height:92vh/.test(indexHtml), 'conversation detail modal needs a fixed viewport height for reliable scrolling');
assert(/\.conv-body\{[^}]*overflow:auto/.test(indexHtml), 'conversation detail body must allow vertical scrolling');
assert(!/\.conv-body\{[^}]*overflow:hidden/.test(indexHtml), 'conversation detail body must not clip long content');
assert(/class="conv-summary"/.test(indexHtml), 'conversation detail view should render a visual summary strip');
assert(/data-mode="reader"/.test(indexHtml), 'conversation detail view should include a Typora-style reader mode');
assert(/\.conv-reader-shell/.test(indexHtml), 'reader mode should define a document plus outline layout');
assert(/\.conv-outline/.test(indexHtml), 'reader mode should include an outline sidebar');
assert(/function extractConversationText\(/.test(indexHtml), 'reader mode needs conversation text extraction');
assert(/function renderMarkdownDocument\(/.test(indexHtml), 'reader mode needs markdown rendering');
assert(/function renderInlineMarkdown\(/.test(indexHtml), 'reader mode needs inline markdown rendering');
assert(/function renderMathFormula\(/.test(indexHtml), 'reader mode needs formula rendering');
assert(/function buildMarkdownOutline\(/.test(indexHtml), 'reader mode needs heading outline generation');
assert(/join\('<br>'\)/.test(indexHtml), 'markdown renderer should preserve single newlines as visible line breaks');
assert(/\.math-inline/.test(indexHtml) && /\.math-block/.test(indexHtml), 'formula rendering should include inline and block math styles');
assert(/data\.output_raw\.split/.test(indexHtml), 'reader mode should extract readable text from streaming output_raw');
assert(/choices\?\.\[0\]\?\.delta\?\.content/.test(indexHtml), 'streaming output extraction should read delta content chunks');

for (const [name, html] of [['index.html', indexHtml], ['login.html', loginHtml]]) {
  const scripts = [...html.matchAll(/<script(?![^>]*\bsrc=)[^>]*>([\s\S]*?)<\/script>/gi)];
  for (const [i, match] of scripts.entries()) {
    assert.doesNotThrow(() => new Function(match[1]), `${name} inline script ${i + 1} should parse`);
  }
}

console.log('frontend static checks passed');
