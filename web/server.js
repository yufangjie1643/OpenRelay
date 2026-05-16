const express = require('express');
const fs = require('fs');
const path = require('path');
const yaml = require('js-yaml');
const bcrypt = require('bcryptjs');
const jwt = require('jsonwebtoken');
const crypto = require('crypto');
const { spawn, exec } = require('child_process');

const app = express();
const PORT = 8080;
const PROJECT_ROOT = path.resolve(__dirname, '..');

// ---------- Active Connections ----------
const activeConnections = new Map();

const CONFIG_JSON = path.join(PROJECT_ROOT, 'config.json');
const CONFIG_YAML = path.join(PROJECT_ROOT, 'litellm-config.yaml');
const USAGE_FILE = path.join(PROJECT_ROOT, 'usage.jsonl');
const JWT_SECRET = process.env.JWT_SECRET || 'litellm-webui-secret-change-me';

app.use(express.json({ limit: '50mb' }));
app.use(express.static(path.join(__dirname, 'public')));

// ---------- Helpers ----------

function loadConfig() {
  const raw = fs.readFileSync(CONFIG_JSON, 'utf8');
  const cfg = JSON.parse(raw);
  if (!cfg.conversation_storage) {
    cfg.conversation_storage = { enabled: false, directory: '' };
  }
  return migrateIfNeeded(cfg);
}

function saveConfig(cfg) {
  fs.writeFileSync(CONFIG_JSON, JSON.stringify(cfg, null, 2), 'utf8');
  const flat = flattenProvidersToLiteLLM(cfg);
  fs.writeFileSync(CONFIG_YAML, yaml.dump(flat, { lineWidth: -1 }), 'utf8');
}

// ---------- LiteLLM Proxy Restart ----------

function getPidByPort(port, callback) {
  exec(`netstat -ano | findstr ":${port}" | findstr "LISTENING"`, (err, stdout) => {
    if (err || !stdout) return callback(null);
    const match = stdout.match(/LISTENING\s+(\d+)/);
    callback(match ? parseInt(match[1]) : null);
  });
}

function waitForPortFree(port, timeoutMs, callback) {
  const start = Date.now();
  const check = () => {
    getPidByPort(port, (pid) => {
      if (!pid) return callback(true);
      if (Date.now() - start > timeoutMs) return callback(false);
      setTimeout(check, 500);
    });
  };
  check();
}

function restartLiteLLMProxy(callback) {
  const litellmExe = path.join(PROJECT_ROOT, '.venv', 'Scripts', 'litellm.exe');
  if (!fs.existsSync(litellmExe)) {
    return callback(new Error('LiteLLM executable not found at ' + litellmExe));
  }

  getPidByPort(4000, (pid) => {
    if (pid) {
      try { process.kill(pid); } catch (e) { console.log('[Restart] Kill warning:', e.message); }
    }

    waitForPortFree(4000, 10000, (freed) => {
      if (!freed) console.log('[Restart] Warning: port 4000 still in use, starting anyway');

      const child = spawn(litellmExe, ['--config', CONFIG_YAML, '--port', '4000'], {
        detached: true,
        stdio: 'ignore',
        cwd: PROJECT_ROOT,
        windowsHide: true
      });
      child.unref();
      console.log(`[Restart] LiteLLM proxy started (PID: ${child.pid})`);
      callback(null, child.pid);
    });
  });
}

// Migrate old flat model_list to provider-centric structure
function migrateIfNeeded(cfg) {
  if (cfg.providers && Array.isArray(cfg.providers)) return cfg;
  if (!cfg.model_list || !Array.isArray(cfg.model_list)) return cfg;

  const providers = [];
  const groups = {};

  for (const item of cfg.model_list) {
    const p = item.litellm_params;
    const base = p.api_base || '';
    const key = p.api_key || '';
    const ua = (p.extra_headers && p.extra_headers['User-Agent']) || 'OpenClaw-Gateway/1.0';
    const realModel = p.model || '';
    const parts = realModel.split('/');
    const type = parts[0] || 'openai';
    const modelId = parts.slice(1).join('/') || realModel;

    const groupKey = `${type}|${base}|${key}|${ua}`;
    if (!groups[groupKey]) {
      groups[groupKey] = {
        id: crypto.randomUUID(),
        name: guessProviderName(type, base),
        type: type === 'openai' && base ? 'openai-custom' : type,
        base_url: base,
        api_key: key,
        user_agent: ua,
        models: []
      };
    }
    groups[groupKey].models.push({
      model_name: item.model_name,
      model_id: modelId
    });
  }

  for (const g of Object.values(groups)) {
    providers.push(g);
  }

  const migrated = { ...cfg, providers };
  delete migrated.model_list;
  saveConfig(migrated);
  console.log('[Migration] Converted flat model_list to provider-centric structure');
  return migrated;
}

function guessProviderName(type, baseUrl) {
  if (baseUrl.includes('minimaxi')) return 'MiniMax';
  if (baseUrl.includes('moonshot')) return 'Moonshot (Kimi)';
  if (baseUrl.includes('deepseek')) return 'DeepSeek';
  if (type === 'anthropic') return 'Anthropic (Claude)';
  if (type === 'gemini') return 'Google Gemini';
  if (type === 'openai') return 'OpenAI';
  return type;
}

// Flatten providers into LiteLLM-compatible model_list for YAML generation
function flattenProvidersToLiteLLM(cfg) {
  const out = { ...cfg };
  delete out.providers;
  delete out.admin;
  delete out.pricing;
  delete out.limits;
  delete out.virtual_keys;
  out.model_list = [];

  for (const prov of cfg.providers || []) {
    for (const m of prov.models || []) {
      const prefix = prov.type === 'gemini' ? 'gemini' : prov.type === 'anthropic' ? 'anthropic' : 'openai';
      const entry = {
        model_name: m.model_name,
        litellm_params: {
          model: `${prefix}/${m.model_id}`,
          api_key: prov.api_key,
          extra_headers: { 'User-Agent': prov.user_agent || 'OpenClaw-Gateway/1.0' }
        }
      };
      if (prov.base_url) entry.litellm_params.api_base = prov.base_url;
      out.model_list.push(entry);
    }
  }

  return out;
}

// Derive flat model list from providers for internal APIs
function getModelListFromProviders(cfg) {
  const list = [];
  for (const prov of cfg.providers || []) {
    for (const m of prov.models || []) {
      const prefix = prov.type === 'gemini' ? 'gemini' : prov.type === 'anthropic' ? 'anthropic' : 'openai';
      list.push({
        model_name: m.model_name,
        litellm_params: {
          model: `${prefix}/${m.model_id}`,
          api_key: prov.api_key,
          api_base: prov.base_url || undefined,
          extra_headers: { 'User-Agent': prov.user_agent || 'OpenClaw-Gateway/1.0' }
        }
      });
    }
  }
  return list;
}

function ensureFiles() {
  if (!fs.existsSync(CONFIG_JSON)) {
    const defaultCfg = {
      admin: { username: 'admin', password_hash: bcrypt.hashSync('admin123', 10) },
      providers: [],
      pricing: {},
      limits: {},
      virtual_keys: [],
      router_settings: { timeout: 60 },
      litellm_settings: { drop_params: true, allowed_headers: ['*'] },
      general_settings: { master_key: 'sk-litellm-master-key' }
    };
    saveConfig(defaultCfg);
  }
  if (!fs.existsSync(USAGE_FILE)) fs.writeFileSync(USAGE_FILE, '', 'utf8');
  const cfg = loadConfig();
  if (!cfg.admin) { cfg.admin = { username: 'admin', password_hash: bcrypt.hashSync('admin123', 10) }; saveConfig(cfg); }
  if (!cfg.pricing) { cfg.pricing = {}; saveConfig(cfg); }
  if (!cfg.limits) { cfg.limits = {}; saveConfig(cfg); }
  if (!cfg.virtual_keys) { cfg.virtual_keys = []; saveConfig(cfg); }
  if (!cfg.providers) { cfg.providers = []; saveConfig(cfg); }
}

// In-memory usage index
let usageIndex = [];
const requestTimestamps = {};

function loadUsageIndex() {
  usageIndex = [];
  if (!fs.existsSync(USAGE_FILE)) return;
  const lines = fs.readFileSync(USAGE_FILE, 'utf8').trim().split('\n').filter(Boolean);
  for (const line of lines) {
    try { usageIndex.push(JSON.parse(line)); } catch {}
  }
}

function appendUsage(entry) {
  const line = JSON.stringify(entry) + '\n';
  fs.appendFileSync(USAGE_FILE, line, 'utf8');
  usageIndex.push(entry);
}

function estimateTokens(messages) {
  if (!messages) return 0;
  let chars = 0;
  for (const m of messages) { if (m.content) chars += String(m.content).length; }
  return Math.ceil(chars / 4);
}

function calcCost(model, inputTokens, outputTokens, pricingTable, cachedTokens = 0, cachedWriteTokens = 0) {
  const p = pricingTable[model] || { input: 0, cached_input: 0, cached_write: 0, output: 0 };
  const normalInput = Math.max(0, inputTokens - cachedTokens);
  return ((normalInput / 1000000) * (p.input || 0))
       + ((cachedTokens / 1000000) * (p.cached_input || p.input || 0))
       + ((cachedWriteTokens / 1000000) * (p.cached_write || p.input || 0))
       + ((outputTokens / 1000000) * (p.output || 0));
}

function getModelUsageInWindow(model, windowMs) {
  const cutoff = Date.now() - windowMs;
  return usageIndex.filter(u => u.model === model && u.timestamp >= cutoff);
}

function getVirtualKeyUsage(keyValue) {
  return usageIndex.filter(u => u.api_key === keyValue);
}

function checkLimits(cfg, model, estimatedInput, estimatedOutput) {
  const modelLimit = cfg.limits[model];
  const globalLimit = cfg.limits.global;
  const cost = calcCost(model, estimatedInput, estimatedOutput, cfg.pricing || {});

  if (modelLimit?.rpm) {
    const recent = getModelUsageInWindow(model, 60000);
    if (recent.length >= modelLimit.rpm) {
      return { allowed: false, reason: `模型 ${model} 请求频率超限 (RPM: ${modelLimit.rpm})` };
    }
  }

  if (modelLimit?.budget) {
    const modelUsage = usageIndex.filter(u => u.model === model);
    const spent = modelUsage.reduce((sum, u) => sum + (u.cost || 0), 0);
    if (spent + cost > modelLimit.budget) {
      return { allowed: false, reason: `模型 ${model} 预算已用尽 (上限: ${modelLimit.budget} 元)` };
    }
  }

  if (globalLimit?.budget) {
    const totalSpent = usageIndex.reduce((sum, u) => sum + (u.cost || 0), 0);
    if (totalSpent + cost > globalLimit.budget) {
      return { allowed: false, reason: `全局预算已用尽 (上限: ${globalLimit.budget} 元)` };
    }
  }

  return { allowed: true, estimatedCost: cost };
}

function checkVirtualKey(cfg, authKey, model) {
  if (!authKey) return { allowed: false, reason: '缺少 API Key' };
  const masterKey = cfg.general_settings?.master_key;
  if (authKey === masterKey) return { allowed: true, isMaster: true };

  const vk = cfg.virtual_keys.find(k => k.key === authKey && k.enabled !== false);
  if (!vk) return { allowed: false, reason: '无效的 API Key' };

  if (vk.expires_at && new Date(vk.expires_at) < new Date()) {
    return { allowed: false, reason: `密钥 "${vk.name}" 已过期` };
  }

  if (model && vk.allowed_models && vk.allowed_models.length > 0 && !vk.allowed_models.includes(model)) {
    return { allowed: false, reason: `密钥 "${vk.name}" 无权访问模型 ${model}` };
  }

  if (vk.budget) {
    const used = getVirtualKeyUsage(vk.key).reduce((sum, u) => sum + (u.cost || 0), 0);
    if (used >= vk.budget) {
      return { allowed: false, reason: `密钥 "${vk.name}" 预算已用尽` };
    }
  }

  if (vk.rpm) {
    const recent = usageIndex.filter(u => u.api_key === vk.key && u.timestamp >= Date.now() - 60000);
    if (recent.length >= vk.rpm) {
      return { allowed: false, reason: `密钥 "${vk.name}" 请求频率超限 (RPM: ${vk.rpm})` };
    }
  }

  return { allowed: true, isMaster: false, virtualKey: vk };
}

// ---------- Auth ----------

function authMiddleware(req, res, next) {
  const auth = req.headers['authorization'];
  if (!auth || !auth.startsWith('Bearer ')) return res.status(401).json({ error: 'Unauthorized' });
  const token = auth.slice(7);
  try { req.user = jwt.verify(token, JWT_SECRET); next(); }
  catch { res.status(401).json({ error: 'Invalid token' }); }
}

app.post('/api/login', async (req, res) => {
  const { username, password } = req.body;
  const cfg = loadConfig();
  if (!cfg.admin || username !== cfg.admin.username) return res.status(401).json({ error: 'Invalid credentials' });
  const ok = await bcrypt.compare(password, cfg.admin.password_hash);
  if (!ok) return res.status(401).json({ error: 'Invalid credentials' });
  const token = jwt.sign({ username }, JWT_SECRET, { expiresIn: '7d' });
  res.json({ success: true, token });
});

app.post('/api/change-password', authMiddleware, async (req, res) => {
  const { oldPassword, newPassword } = req.body;
  const cfg = loadConfig();
  const ok = await bcrypt.compare(oldPassword, cfg.admin.password_hash);
  if (!ok) return res.status(400).json({ error: 'Old password incorrect' });
  cfg.admin.password_hash = bcrypt.hashSync(newPassword, 10);
  saveConfig(cfg);
  res.json({ success: true });
});

// ---------- Config ----------

app.get('/api/config', authMiddleware, (req, res) => {
  try {
    const cfg = loadConfig();
    const safe = { ...cfg };
    if (safe.admin) safe.admin = { username: safe.admin.username };
    safe.virtual_keys = (safe.virtual_keys || []).map(k => ({ ...k, key: k.key.slice(0, 8) + '****' + k.key.slice(-4) }));
    res.json(safe);
  } catch (e) { res.status(500).json({ error: e.message }); }
});

app.post('/api/config', authMiddleware, (req, res) => {
  try {
    const current = loadConfig();
    const incoming = req.body;
    incoming.admin = current.admin;
    incoming.virtual_keys = current.virtual_keys;
    saveConfig(incoming);
    res.json({ success: true });
  } catch (e) { res.status(500).json({ error: e.message }); }
});

app.post('/api/restart-proxy', authMiddleware, (req, res) => {
  restartLiteLLMProxy((err, pid) => {
    if (err) return res.status(500).json({ success: false, error: err.message });
    res.json({ success: true, pid });
  });
});

// ---------- Pricing ----------

app.get('/api/pricing', authMiddleware, (req, res) => {
  res.json(loadConfig().pricing || {});
});

app.post('/api/pricing', authMiddleware, (req, res) => {
  const cfg = loadConfig();
  cfg.pricing = req.body;
  saveConfig(cfg);
  res.json({ success: true });
});

// ---------- Limits ----------

app.get('/api/limits', authMiddleware, (req, res) => {
  res.json(loadConfig().limits || {});
});

app.post('/api/limits', authMiddleware, (req, res) => {
  const cfg = loadConfig();
  cfg.limits = req.body;
  saveConfig(cfg);
  res.json({ success: true });
});

// ---------- Virtual Keys ----------

app.get('/api/virtual-keys', authMiddleware, (req, res) => {
  const cfg = loadConfig();
  const keys = (cfg.virtual_keys || []).map(k => ({ ...k, key: k.key.slice(0, 8) + '****' + k.key.slice(-4) }));
  res.json(keys);
});

app.post('/api/virtual-keys', authMiddleware, (req, res) => {
  const cfg = loadConfig();
  if (!cfg.virtual_keys) cfg.virtual_keys = [];
  const newKey = { ...req.body, key: 'sk-vk-' + crypto.randomBytes(16).toString('hex') };
  cfg.virtual_keys.push(newKey);
  saveConfig(cfg);
  res.json({ success: true, key: newKey });
});

app.put('/api/virtual-keys/:index', authMiddleware, (req, res) => {
  const cfg = loadConfig();
  const idx = parseInt(req.params.index);
  if (!cfg.virtual_keys || idx < 0 || idx >= cfg.virtual_keys.length) return res.status(404).json({ error: 'Not found' });
  const existingKey = cfg.virtual_keys[idx].key;
  cfg.virtual_keys[idx] = { ...req.body, key: existingKey };
  saveConfig(cfg);
  res.json({ success: true });
});

app.delete('/api/virtual-keys/:index', authMiddleware, (req, res) => {
  const cfg = loadConfig();
  const idx = parseInt(req.params.index);
  if (!cfg.virtual_keys || idx < 0 || idx >= cfg.virtual_keys.length) return res.status(404).json({ error: 'Not found' });
  cfg.virtual_keys.splice(idx, 1);
  saveConfig(cfg);
  res.json({ success: true });
});

// ---------- Detect Models ----------

function resolveApiKey(keyRef) {
  if (typeof keyRef !== 'string') return '';
  if (keyRef.startsWith('os.environ/')) {
    const envName = keyRef.slice('os.environ/'.length);
    return process.env[envName] || '';
  }
  return keyRef;
}

function getBaseUrlForProvider(type, base_url) {
  if (base_url) return base_url;
  if (type === 'gemini') return 'https://generativelanguage.googleapis.com/v1beta';
  if (type === 'anthropic') return 'https://api.anthropic.com/v1';
  return 'https://api.openai.com/v1';
}

app.post('/api/test-provider', authMiddleware, async (req, res) => {
  const { type, base_url, api_key, user_agent } = req.body;
  const baseUrl = getBaseUrlForProvider(type, base_url);
  const key = resolveApiKey(api_key);

  if (!key) return res.json({ success: false, error: 'API Key 为空' });
  if (key === 'test' || key.includes('your-') || key.includes('placeholder')) {
    return res.json({ success: false, error: 'API Key 为占位符，请填写真实密钥' });
  }

  try {
    let url, headers = { 'User-Agent': user_agent || 'OpenClaw-Gateway/1.0' };
    if (type === 'gemini') {
      url = `${baseUrl}/models?key=${key}`;
    } else {
      url = `${baseUrl}/models`;
      headers['Authorization'] = `Bearer ${key}`;
    }

    const response = await fetch(url, { headers, signal: AbortSignal.timeout(15000) });
    if (!response.ok) {
      const text = await response.text().catch(() => '');
      return res.json({ success: false, error: `HTTP ${response.status}: ${text.slice(0, 200)}` });
    }

    const data = await response.json();
    let models = [];
    if (type === 'gemini' && Array.isArray(data.models)) {
      models = data.models.map(m => ({ id: (m.name || '').replace('models/', ''), owned_by: m.displayName || 'google' }));
    } else if (Array.isArray(data.data)) {
      models = data.data.map(m => ({ id: m.id, owned_by: m.owned_by || '' }));
    } else if (Array.isArray(data.object) && data.object === 'list' && Array.isArray(data.data)) {
      models = data.data.map(m => ({ id: m.id, owned_by: m.owned_by || '' }));
    }

    res.json({ success: true, models, base_url: baseUrl });
  } catch (e) {
    res.json({ success: false, error: e.message });
  }
});

app.get('/api/detect-models', authMiddleware, async (req, res) => {
  const cfg = loadConfig();
  const detected = [];
  const seen = new Set();
  const errors = [];

  for (const prov of cfg.providers || []) {
    const baseUrl = prov.base_url || 'https://api.openai.com/v1';
    const apiKey = resolveApiKey(prov.api_key);
    const provider = prov.type === 'openai-custom' ? 'openai' : prov.type;

    if (!apiKey || apiKey === 'test' || apiKey.includes('your-') || apiKey.includes('placeholder')) {
      continue;
    }

    const cacheKey = baseUrl + '|' + apiKey.slice(0, 8);
    if (seen.has(cacheKey)) continue;
    seen.add(cacheKey);

    try {
      const response = await fetch(`${baseUrl}/models`, {
        headers: { 'Authorization': `Bearer ${apiKey}`, 'User-Agent': prov.user_agent || 'OpenClaw-Gateway/1.0' },
        signal: AbortSignal.timeout(15000)
      });
      if (!response.ok) {
        errors.push({ provider, baseUrl, status: response.status });
        continue;
      }
      const data = await response.json();
      for (const m of data.data || []) {
        const localAlias = (cfg.providers || []).flatMap(p => p.models || []).find(
          lm => `${provider}/${lm.model_id}` === `${provider}/${m.id}` || lm.model_id === m.id
        )?.model_name;
        detected.push({
          id: m.id,
          provider,
          base_url: baseUrl,
          owned_by: m.owned_by || '',
          local_alias: localAlias || null
        });
      }
    } catch (e) {
      errors.push({ provider, baseUrl, error: e.message });
    }
  }

  res.json({ models: detected, errors, count: detected.length });
});

// ---------- Usage / Stats ----------

app.get('/api/usage', authMiddleware, (req, res) => {
  const model = req.query.model || null;
  const page = Math.max(1, parseInt(req.query.page) || 1);
  const pageSize = Math.max(1, Math.min(500, parseInt(req.query.pageSize) || 20));
  const entries = model ? usageIndex.filter(u => u.model === model) : usageIndex;
  const lastEntries = entries.slice(-10000);
  const stats = {
    totalRequests: lastEntries.length,
    totalInputTokens: 0,
    totalCachedTokens: 0,
    totalOutputTokens: 0,
    totalCost: 0,
    cacheHitRate: 0,
    byModel: {},
    byKey: {}
  };
  for (const e of lastEntries) {
    stats.totalInputTokens += e.input_tokens || 0;
    stats.totalCachedTokens += e.cached_tokens || 0;
    stats.totalOutputTokens += e.output_tokens || 0;
    stats.totalCost += e.cost || 0;
    if (!stats.byModel[e.model]) stats.byModel[e.model] = { requests: 0, input_tokens: 0, cached_tokens: 0, output_tokens: 0, cost: 0 };
    stats.byModel[e.model].requests++;
    stats.byModel[e.model].input_tokens += e.input_tokens || 0;
    stats.byModel[e.model].cached_tokens += e.cached_tokens || 0;
    stats.byModel[e.model].output_tokens += e.output_tokens || 0;
    stats.byModel[e.model].cost += e.cost || 0;
    const keyName = e.key_name || 'master';
    if (!stats.byKey[keyName]) stats.byKey[keyName] = { requests: 0, cost: 0 };
    stats.byKey[keyName].requests++;
    stats.byKey[keyName].cost += e.cost || 0;
  }
  if (stats.totalInputTokens > 0) {
    stats.cacheHitRate = ((stats.totalCachedTokens / stats.totalInputTokens) * 100).toFixed(2);
  }
  // Pagination: newest first
  const allEntries = entries.slice().reverse();
  const total = allEntries.length;
  const totalPages = Math.max(1, Math.ceil(total / pageSize));
  const currentPage = Math.min(page, totalPages);
  const offset = (currentPage - 1) * pageSize;
  const pagedEntries = allEntries.slice(offset, offset + pageSize);
  res.json({ entries: pagedEntries, stats, pagination: { total, page: currentPage, pageSize, totalPages } });
});

app.post('/api/usage/clear', authMiddleware, (req, res) => {
  try {
    fs.writeFileSync(USAGE_FILE, '', 'utf8');
    usageIndex = [];
    res.json({ success: true });
  } catch (e) { res.status(500).json({ error: e.message }); }
});

app.post('/api/usage/merge', authMiddleware, (req, res) => {
  try {
    const archiveFile = path.join(PROJECT_ROOT, 'usage-archive-' + new Date().toISOString().slice(0,10) + '.jsonl');
    if (fs.existsSync(USAGE_FILE) && fs.statSync(USAGE_FILE).size > 0) {
      fs.copyFileSync(USAGE_FILE, archiveFile);
      fs.writeFileSync(USAGE_FILE, '', 'utf8');
      usageIndex = [];
    }
    res.json({ success: true, archive: archiveFile });
  } catch (e) { res.status(500).json({ error: e.message }); }
});

app.get('/api/usage/export', authMiddleware, (req, res) => {
  try {
    const format = req.query.format || 'csv';
    if (format !== 'csv') return res.status(400).json({ error: '仅支持 csv 格式' });
    const lines = fs.existsSync(USAGE_FILE) ? fs.readFileSync(USAGE_FILE, 'utf8').trim().split('\n').filter(Boolean) : [];
    const columns = ['request_id','timestamp','model','key_name','input_tokens','cached_tokens','cached_write_tokens','output_tokens','cost','duration_ms','status','stream','user_agent','error'];
    const escapeCsv = (val) => {
      if (val === undefined || val === null) return '';
      const str = String(val);
      if (str.includes(',') || str.includes('\n') || str.includes('"')) return '"' + str.replace(/"/g, '""') + '"';
      return str;
    };
    let csv = columns.map(c => escapeCsv(c)).join(',') + '\n';
    for (const line of lines) {
      try {
        const entry = JSON.parse(line);
        csv += columns.map(c => escapeCsv(entry[c])).join(',') + '\n';
      } catch {}
    }
    res.setHeader('Content-Type', 'text/csv; charset=utf-8');
    res.setHeader('Content-Disposition', 'attachment; filename="usage-export-' + new Date().toISOString().slice(0,10) + '.csv"');
    res.send(csv);
  } catch (e) { res.status(500).json({ error: e.message }); }
});

function resolveProvider(model, cfg) {
  for (const provider of cfg.providers || []) {
    const found = (provider.models || []).find(m => m.model_name === model);
    if (found) {
      return { ...provider, model_id: found.model_id || found.model_name };
    }
  }
  return null;
}

function saveConversation(cfg, entry) {
  const cs = cfg.conversation_storage;
  if (!cs || !cs.enabled || !cs.directory) return;
  try {
    const dir = cs.directory;
    if (!fs.existsSync(dir)) fs.mkdirSync(dir, { recursive: true });
    const filename = `conv-${entry.timestamp}-${entry.request_id}.json`;
    const filepath = path.join(dir, filename);
    fs.writeFileSync(filepath, JSON.stringify(entry, null, 2), 'utf8');
  } catch (e) {
    console.error('[Conversation] Save failed:', e.message);
  }
}

// ---------- Proxy with Logging & Limits ----------

async function proxyRequest(req, res, targetPath) {
  const requestId = crypto.randomUUID();
  const body = req.body || {};
  const model = body.model || 'unknown';
  const isStream = body.stream === true;
  const estimatedInput = estimateTokens(body.messages);
  const startTime = Date.now();
  const cfg = loadConfig();

  const authHeader = req.headers['authorization'] || '';
  const authKey = authHeader.startsWith('Bearer ') ? authHeader.slice(7) : '';
  const isModelsEndpoint = targetPath === '/v1/models' || targetPath.endsWith('/models');
  const keyCheck = checkVirtualKey(cfg, authKey, isModelsEndpoint ? null : model);
  if (!keyCheck.allowed) {
    return res.status(429).json({ error: { message: keyCheck.reason, type: 'limit_error', code: 429 } });
  }

  if (!isModelsEndpoint) {
    const limitCheck = checkLimits(cfg, model, estimatedInput, 0);
    if (!limitCheck.allowed) {
      return res.status(429).json({ error: { message: limitCheck.reason, type: 'limit_error', code: 429 } });
    }
  }

  const keyName = keyCheck.isMaster ? 'master' : (keyCheck.virtualKey?.name || 'unknown');
  const abortController = new AbortController();

  activeConnections.set(requestId, {
    requestId, startTime, model, keyName,
    userAgent: req.headers['user-agent'] || '',
    stream: isStream, targetPath,
    abortController
  });

  try {
    let response;
    if (isModelsEndpoint) {
      response = await fetch(`http://localhost:4000${targetPath}`, {
        method: req.method,
        headers: {
          'Content-Type': 'application/json',
          'Authorization': `Bearer ${cfg.general_settings?.master_key || ''}`
        },
        body: req.method !== 'GET' ? JSON.stringify(body) : undefined,
        signal: abortController.signal
      });
    } else {
      const provider = resolveProvider(model, cfg);
      if (!provider) {
        return res.status(400).json({ error: { message: `未知模型: ${model}`, type: 'invalid_request_error', code: 400 } });
      }
      const upstreamUrl = `${provider.base_url.replace(/\/$/, '')}${targetPath.replace(/^\/v1/, '')}`;
      const upstreamHeaders = {
        'Content-Type': 'application/json',
        'Authorization': `Bearer ${provider.api_key}`
      };
      if (provider.user_agent) {
        upstreamHeaders['User-Agent'] = provider.user_agent;
      }
      const upstreamBody = { ...body };
      if (provider.model_id !== model) {
        upstreamBody.model = provider.model_id;
      }
      response = await fetch(upstreamUrl, {
        method: req.method,
        headers: upstreamHeaders,
        body: req.method !== 'GET' ? JSON.stringify(upstreamBody) : undefined,
        signal: abortController.signal
      });
    }

    if (!isStream) {
      const text = await response.text();
      let data;
      try { data = JSON.parse(text); } catch (parseErr) {
        console.error('[Proxy] Upstream non-JSON response:', text.slice(0, 500));
        return res.status(502).json({ error: 'Proxy error', message: '上游返回非 JSON 响应: ' + parseErr.message });
      }
      if (isModelsEndpoint && data.data && Array.isArray(data.data) && !keyCheck.isMaster && keyCheck.virtualKey?.allowed_models?.length > 0) {
        data.data = data.data.filter(m => keyCheck.virtualKey.allowed_models.includes(m.id));
      }
      const usage = data?.usage;
      const inputTokens = usage?.prompt_tokens || estimatedInput;
      const outputTokens = usage?.completion_tokens || 0;
      const cachedTokens = usage?.prompt_tokens_details?.cached_tokens || 0;
      const cachedWriteTokens = usage?.prompt_tokens_details?.cache_write_tokens || 0;
      const cost = calcCost(model, inputTokens, outputTokens, cfg.pricing || {}, cachedTokens, cachedWriteTokens);
      appendUsage({ request_id: requestId, timestamp: Date.now(), model, input_tokens: inputTokens, cached_tokens: cachedTokens, cached_write_tokens: cachedWriteTokens, output_tokens: outputTokens, cost, duration_ms: Date.now() - startTime, status: response.status, stream: false, api_key: authKey, key_name: keyName, user_agent: req.headers['user-agent'] || '' });
      res.status(response.status).json(data);
      if (!isModelsEndpoint) {
        saveConversation(cfg, { request_id: requestId, timestamp: Date.now(), model, key_name: keyName, user_agent: req.headers['user-agent'] || '', input: body, output: data });
      }
    } else {
      res.status(response.status);
      res.setHeader('Content-Type', response.headers.get('content-type') || 'text/event-stream');
      if (response.headers.get('cache-control')) res.setHeader('Cache-Control', response.headers.get('cache-control'));
      const reader = response.body.getReader();
      let outputChunks = 0;
      const streamChunks = [];
      while (true) {
        const { done, value } = await reader.read();
        if (done) break;
        outputChunks += value.length;
        streamChunks.push(Buffer.from(value).toString('utf8'));
        res.write(Buffer.from(value));
      }
      res.end();
      const estimatedOutput = Math.ceil(outputChunks / 16);
      const cost = calcCost(model, estimatedInput, estimatedOutput, cfg.pricing || {}, 0, 0);
      appendUsage({ request_id: requestId, timestamp: Date.now(), model, input_tokens: estimatedInput, cached_tokens: 0, cached_write_tokens: 0, output_tokens: estimatedOutput, cost, duration_ms: Date.now() - startTime, status: response.status, stream: true, api_key: authKey, key_name: keyName, user_agent: req.headers['user-agent'] || '' });
      saveConversation(cfg, { request_id: requestId, timestamp: Date.now(), model, key_name: keyName, user_agent: req.headers['user-agent'] || '', input: body, output_raw: streamChunks.join('') });
    }
  } catch (err) {
    const isAbort = err.name === 'AbortError' || err.message?.includes('aborted');
    const status = isAbort ? 499 : 502;
    if (!res.headersSent) {
      res.status(status).json({ error: isAbort ? 'Request aborted by admin' : 'Proxy error', message: err.message });
    } else if (!res.writableEnded) {
      res.end();
    }
    appendUsage({ request_id: requestId, timestamp: Date.now(), model, input_tokens: estimatedInput, cached_tokens: 0, cached_write_tokens: 0, output_tokens: 0, cost: 0, duration_ms: Date.now() - startTime, status, stream: isStream, error: err.message, api_key: authKey, key_name: keyName, user_agent: req.headers['user-agent'] || '' });
  } finally {
    activeConnections.delete(requestId);
  }
}

app.all('/proxy/v1/*', (req, res) => proxyRequest(req, res, req.path.replace('/proxy', '')));
app.all('/proxy/*', (req, res) => proxyRequest(req, res, req.path.replace('/proxy', '')));

// ---------- Init ----------

ensureFiles();
loadUsageIndex();

app.listen(PORT, '0.0.0.0', () => {
  console.log(`LiteLLM Web UI + Proxy running at http://localhost:${PORT}`);
  console.log(`Proxy endpoint: http://localhost:${PORT}/proxy/v1/chat/completions`);
  initTray();
});

// ---------- Conversations ----------

app.get('/api/conversations', authMiddleware, (req, res) => {
  try {
    const cfg = loadConfig();
    const cs = cfg.conversation_storage;
    if (!cs || !cs.enabled || !cs.directory) return res.json({ enabled: false, entries: [] });
    const dir = cs.directory;
    if (!fs.existsSync(dir)) return res.json({ enabled: true, entries: [] });
    const files = fs.readdirSync(dir).filter(f => f.startsWith('conv-') && f.endsWith('.json')).sort().reverse();
    const entries = files.map(f => {
      try {
        const content = JSON.parse(fs.readFileSync(path.join(dir, f), 'utf8'));
        return { filename: f, timestamp: content.timestamp, model: content.model, key_name: content.key_name || 'master', request_id: content.request_id };
      } catch { return null; }
    }).filter(Boolean);
    res.json({ enabled: true, directory: dir, entries });
  } catch (e) { res.status(500).json({ error: e.message }); }
});

app.get('/api/conversations/:filename', authMiddleware, (req, res) => {
  try {
    const cfg = loadConfig();
    const cs = cfg.conversation_storage;
    if (!cs || !cs.enabled || !cs.directory) return res.status(400).json({ error: '对话存储未启用' });
    const filepath = path.join(cs.directory, req.params.filename);
    if (!fs.existsSync(filepath)) return res.status(404).json({ error: '文件不存在' });
    const content = JSON.parse(fs.readFileSync(filepath, 'utf8'));
    res.json(content);
  } catch (e) { res.status(500).json({ error: e.message }); }
});

app.delete('/api/conversations/:filename', authMiddleware, (req, res) => {
  try {
    const cfg = loadConfig();
    const cs = cfg.conversation_storage;
    if (!cs || !cs.enabled || !cs.directory) return res.status(400).json({ error: '对话存储未启用' });
    const filepath = path.join(cs.directory, req.params.filename);
    if (!fs.existsSync(filepath)) return res.status(404).json({ error: '文件不存在' });
    fs.unlinkSync(filepath);
    res.json({ success: true });
  } catch (e) { res.status(500).json({ error: e.message }); }
});

app.post('/api/conversation-storage', authMiddleware, (req, res) => {
  try {
    const cfg = loadConfig();
    const { enabled, directory } = req.body;
    cfg.conversation_storage = { enabled: !!enabled, directory: directory || '' };
    saveConfig(cfg);
    res.json({ success: true, conversation_storage: cfg.conversation_storage });
  } catch (e) { res.status(500).json({ error: e.message }); }
});

app.get('/api/connections', authMiddleware, (req, res) => {
  const now = Date.now();
  const entries = Array.from(activeConnections.values()).map(c => ({
    requestId: c.requestId,
    startTime: c.startTime,
    elapsedMs: now - c.startTime,
    model: c.model,
    keyName: c.keyName,
    userAgent: c.userAgent,
    stream: c.stream,
    targetPath: c.targetPath
  }));
  res.json({ count: entries.length, entries });
});

app.post('/api/connections/:id/abort', authMiddleware, (req, res) => {
  const conn = activeConnections.get(req.params.id);
  if (!conn) return res.status(404).json({ error: '连接不存在或已完成' });
  try {
    conn.abortController.abort();
    res.json({ success: true });
  } catch (e) { res.status(500).json({ error: e.message }); }
});

// ---------- System Tray ----------

function initTray() {
  try {
    const SysTray = require('systray').default;
    const iconPath = path.join(__dirname, 'icon.ico');
    let iconBase64 = '';
    if (fs.existsSync(iconPath)) {
      iconBase64 = fs.readFileSync(iconPath).toString('base64');
    }

    const systray = new SysTray({
      menu: {
        icon: iconBase64,
        title: 'LiteLLM Proxy',
        tooltip: 'LiteLLM Proxy 运行中',
        items: [
          { title: '打开 Web UI', tooltip: '在浏览器中打开管理面板', checked: false, enabled: true },
          { title: '重启 LiteLLM 代理', tooltip: '重新加载 litellm-config.yaml', checked: false, enabled: true },
          { title: '退出', tooltip: '关闭所有服务', checked: false, enabled: true }
        ]
      },
      debug: false,
      copyDir: true
    });

    systray.onClick(action => {
      if (action.seq_id === 0) {
        // Open Web UI
        const { exec } = require('child_process');
        exec(`start http://localhost:${PORT}`);
      } else if (action.seq_id === 1) {
        // Restart LiteLLM
        restartLiteLLMProxy((err, pid) => {
          if (err) console.log('[Tray] Restart failed:', err.message);
          else console.log('[Tray] Restarted, PID:', pid);
        });
      } else if (action.seq_id === 2) {
        // Exit
        getPidByPort(4000, pid => {
          if (pid) try { process.kill(pid); } catch {}
          systray.kill();
          process.exit(0);
        });
      }
    });

    // 每30秒更新一次托盘提示，显示总成本和缓存命中率
    setInterval(() => {
      const last = usageIndex.slice(-10000);
      let totalCost = 0, totalInput = 0, totalCached = 0;
      for (const e of last) {
        totalCost += e.cost || 0;
        totalInput += e.input_tokens || 0;
        totalCached += e.cached_tokens || 0;
      }
      const rate = totalInput > 0 ? ((totalCached / totalInput) * 100).toFixed(1) : 0;
      const title = `LiteLLM Proxy | 成本: ¥${totalCost.toFixed(2)} | 缓存: ${rate}%`;
      if (typeof systray.setTitle === 'function') systray.setTitle(title);
    }, 30000);
  } catch (e) {
    console.log('[Tray] Systray init failed:', e.message);
  }
}
