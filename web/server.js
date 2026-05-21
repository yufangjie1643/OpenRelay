const express = require('express');
const fs = require('fs');
const path = require('path');
const yaml = require('js-yaml');
const bcrypt = require('bcryptjs');
const jwt = require('jsonwebtoken');
const crypto = require('crypto');
const { exec, execFile, spawn } = require('child_process');

const app = express();
const PORT = Number(process.env.PORT) || 18783;
const TRAY_ENABLED = process.argv.includes('--tray') || process.env.ENABLE_TRAY === '1';
const PROJECT_ROOT = path.resolve(__dirname, '..');
const MINIMAX_NATIVE_MODELS = new Set([
  'MiniMax-M2.7',
  'MiniMax-M2.7-highspeed',
  'MiniMax-M2.5',
  'MiniMax-M2.5-highspeed',
  'MiniMax-M2.1',
  'MiniMax-M2.1-highspeed',
  'MiniMax-M2',
  'M2-her',
  'speech-2.8-hd',
  'speech-2.8-turbo',
  'speech-2.6-hd',
  'speech-2.6-turbo',
  'speech-02-hd',
  'speech-02-turbo',
  'speech-01-hd',
  'speech-01-turbo',
  'MiniMax-Hailuo-2.3',
  'MiniMax-Hailuo-2.3-Fast',
  'MiniMax-Hailuo-02',
  'T2V-01',
  'I2V-01',
  'I2V-01-live',
  'I2V-01-Director',
  'T2V-01-Director',
  'I2V-01-Subject',
  'S2V-01',
  'image-01',
  'image-01-live',
  'music-2.6',
  'music-cover',
  'music-2.6-free',
  'music-cover-free'
]);
const COMPATIBLE_PROXY_ROUTES = [
  '/chat/completions',
  '/responses',
  '/completions',
  '/embeddings',
  '/messages',
  '/images/*',
  '/audio/*',
  '/moderations',
  '/rerank',
  '/t2a_v2',
  '/image_generation',
  '/video_generation',
  '/music_generation',
  '/files/*'
];
const PROXY_BODY_ROUTES = ['/proxy', '/v1', ...COMPATIBLE_PROXY_ROUTES.map(route => route.replace('/*', ''))];

// ---------- Active Connections ----------
const activeConnections = new Map();

const CONFIG_JSON = path.join(PROJECT_ROOT, 'config.json');
const CONFIG_YAML = path.join(PROJECT_ROOT, 'litellm-config.yaml');
const USAGE_FILE = path.join(PROJECT_ROOT, 'usage.jsonl');
const USAGE_RECENT_DETAIL_LIMIT = 1000;
const JWT_SECRET = process.env.JWT_SECRET || 'litellm-webui-secret-change-me';

app.use(PROXY_BODY_ROUTES, express.raw({ type: '*/*', limit: '200mb' }));
app.use(express.json({ limit: '50mb' }));
app.use('/vendor/js-yaml', express.static(path.join(__dirname, 'node_modules', 'js-yaml', 'dist')));
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
  archiveUsageIndexIfNeeded();
}

function appendUsage(entry) {
  const line = JSON.stringify(entry) + '\n';
  try {
    fs.appendFileSync(USAGE_FILE, line, 'utf8');
  } catch (e) {
    console.error('[Usage] Append failed:', e.message);
  }
  usageIndex.push(entry);
  archiveUsageIndexIfNeeded();
}

function estimateTokens(payload) {
  const collectText = (value) => {
    if (value === undefined || value === null) return '';
    if (typeof value === 'string') return value;
    if (Array.isArray(value)) return value.map(collectText).join(' ');
    if (typeof value === 'object') {
      if (typeof value.text === 'string') return value.text;
      if (typeof value.content === 'string') return value.content;
      if (Array.isArray(value.content)) return collectText(value.content);
      return Object.values(value).map(collectText).join(' ');
    }
    return String(value);
  };
  if (!payload) return 0;
  let text;
  if (Array.isArray(payload)) {
    text = payload.map(m => collectText(m.content ?? m)).join(' ');
  } else if (typeof payload === 'object') {
    text = collectText(payload.messages || payload.input || payload.prompt || payload.instructions || payload);
  } else {
    text = collectText(payload);
  }
  return Math.ceil(text.length / 4);
}

function normalizeProxyPath(targetPath) {
  const raw = targetPath || '/';
  const [pathname, query = ''] = raw.split('?');
  let normalized = pathname.startsWith('/') ? pathname : '/' + pathname;
  normalized = normalized.replace(/\/+/g, '/');
  return { pathname: normalized, query: query ? '?' + query : '' };
}

function isVersionedBaseUrl(baseUrl) {
  try {
    const u = new URL(baseUrl);
    return /\/v\d+(?:\/)?$/i.test(u.pathname);
  } catch {
    return /\/v\d+(?:\/)?$/i.test(baseUrl || '');
  }
}

function buildUpstreamUrl(baseUrl, targetPath, queryString = '') {
  const { pathname, query } = normalizeProxyPath(targetPath);
  const cleanBase = String(baseUrl || '').replace(/\/+$/, '');
  const cleanQuery = queryString || query || '';
  let upstreamPath = pathname;
  if (isVersionedBaseUrl(cleanBase)) {
    upstreamPath = upstreamPath.replace(/^\/v\d+(?=\/|$)/i, '') || '/';
  }
  if (!upstreamPath.startsWith('/')) upstreamPath = '/' + upstreamPath;
  return cleanBase + upstreamPath + cleanQuery;
}

function getContentType(contentType) {
  if (Array.isArray(contentType)) return contentType[0] || '';
  return String(contentType || '');
}

function isJsonContentType(contentType) {
  const normalized = getContentType(contentType).toLowerCase();
  return normalized.includes('/json') || normalized.includes('+json') || normalized.includes('application/json');
}

function isUrlEncodedContentType(contentType) {
  return getContentType(contentType).toLowerCase().includes('application/x-www-form-urlencoded');
}

function isMultipartContentType(contentType) {
  return getContentType(contentType).toLowerCase().includes('multipart/form-data');
}

function looksLikeJsonBuffer(buffer) {
  if (!Buffer.isBuffer(buffer) || buffer.length === 0) return false;
  const first = buffer.toString('utf8', 0, Math.min(buffer.length, 64)).trimStart()[0];
  return first === '{' || first === '[';
}

function getMultipartBoundary(contentType) {
  const match = getContentType(contentType).match(/boundary=(?:"([^"]+)"|([^;]+))/i);
  return match ? (match[1] || match[2] || '').trim() : '';
}

function extractMultipartFields(rawBody, contentType) {
  if (!Buffer.isBuffer(rawBody) || rawBody.length === 0) return {};
  const boundary = getMultipartBoundary(contentType);
  if (!boundary) return {};

  const delimiter = '--' + boundary;
  const raw = rawBody.toString('binary');
  const fields = {};

  for (const part of raw.split(delimiter)) {
    if (!part || part === '--\r\n' || part === '--') continue;
    const headerEnd = part.indexOf('\r\n\r\n');
    if (headerEnd < 0) continue;

    const headers = part.slice(0, headerEnd);
    if (/filename=/i.test(headers)) continue;

    const nameMatch = headers.match(/content-disposition:[^\r\n]*\bname="([^"]+)"/i);
    if (!nameMatch) continue;

    let value = part.slice(headerEnd + 4);
    if (value.endsWith('\r\n')) value = value.slice(0, -2);
    if (value.endsWith('--')) value = value.slice(0, -2);
    fields[nameMatch[1]] = Buffer.from(value, 'binary').toString('utf8');
  }

  return fields;
}

function extractMultipartField(rawBody, contentType, fieldName) {
  const value = extractMultipartFields(rawBody, contentType)[fieldName];
  return typeof value === 'string' ? value.trim() : null;
}

function replaceMultipartField(rawBody, contentType, fieldName, replacement) {
  if (!Buffer.isBuffer(rawBody) || rawBody.length === 0) return rawBody;
  const boundary = getMultipartBoundary(contentType);
  if (!boundary) return rawBody;

  const delimiter = '--' + boundary;
  const raw = rawBody.toString('binary');
  let changed = false;
  const parts = raw.split(delimiter).map(part => {
    const headerEnd = part.indexOf('\r\n\r\n');
    if (headerEnd < 0) return part;

    const headers = part.slice(0, headerEnd);
    if (/filename=/i.test(headers)) return part;

    const nameMatch = headers.match(/content-disposition:[^\r\n]*\bname="([^"]+)"/i);
    if (!nameMatch || nameMatch[1] !== fieldName) return part;

    let ending = '';
    let value = part.slice(headerEnd + 4);
    if (value.endsWith('\r\n')) {
      ending = '\r\n';
      value = value.slice(0, -2);
    }
    changed = true;
    return part.slice(0, headerEnd + 4) + String(replacement) + ending;
  });

  return changed ? Buffer.from(parts.join(delimiter), 'binary') : rawBody;
}

function parseProxyBody(rawBody, contentType = '') {
  if (Buffer.isBuffer(rawBody)) {
    if (rawBody.length === 0) return { body: {}, rawBody };
    if (isJsonContentType(contentType) || (!contentType && looksLikeJsonBuffer(rawBody))) {
      return { body: JSON.parse(rawBody.toString('utf8')), rawBody };
    }
    if (isUrlEncodedContentType(contentType)) {
      return { body: Object.fromEntries(new URLSearchParams(rawBody.toString('utf8'))), rawBody };
    }
    if (isMultipartContentType(contentType)) {
      return { body: extractMultipartFields(rawBody, contentType), rawBody };
    }
    return { body: {}, rawBody };
  }

  if (rawBody && typeof rawBody === 'object') {
    return { body: rawBody, rawBody: Buffer.from(JSON.stringify(rawBody)) };
  }

  return { body: {}, rawBody: Buffer.alloc(0) };
}

function serializeUpstreamBody({ rawBody, body, contentType, requestModel, upstreamModel }) {
  if (!rawBody || rawBody.length === 0) {
    return undefined;
  }

  if (isJsonContentType(contentType) || (!contentType && looksLikeJsonBuffer(rawBody))) {
    const upstreamBody = body && typeof body === 'object' && !Array.isArray(body) ? { ...body } : body;
    if (upstreamBody && typeof upstreamBody === 'object' && !Array.isArray(upstreamBody) && requestModel && upstreamModel !== requestModel) {
      upstreamBody.model = upstreamModel;
    }
    return JSON.stringify(upstreamBody);
  }

  if (isUrlEncodedContentType(contentType) && requestModel && upstreamModel !== requestModel) {
    const params = new URLSearchParams(rawBody.toString('utf8'));
    params.set('model', upstreamModel);
    return params.toString();
  }

  if (isMultipartContentType(contentType) && requestModel && upstreamModel !== requestModel) {
    return replaceMultipartField(rawBody, contentType, 'model', upstreamModel);
  }

  return rawBody;
}

function getRequestModel(body = {}, targetPath = '', rawBody = null, contentType = '') {
  if (body && typeof body.model === 'string' && body.model.trim()) return body.model.trim();
  if (isMultipartContentType(contentType)) {
    const multipartModel = extractMultipartField(rawBody, contentType, 'model');
    if (multipartModel) return multipartModel;
  }
  const { pathname } = normalizeProxyPath(targetPath);
  const modelMatch = pathname.match(/^\/(?:v\d+\/)?models\/([^/?#]+)/i);
  return modelMatch ? decodeURIComponent(modelMatch[1]) : null;
}

function isModelsEndpoint(targetPath = '') {
  const { pathname } = normalizeProxyPath(targetPath);
  return /^\/(?:v\d+\/)?models(?:\/[^/]+)?$/i.test(pathname);
}

function getConfiguredModels(cfg) {
  const seen = new Set();
  const models = [];
  for (const provider of cfg.providers || []) {
    for (const model of provider.models || []) {
      const id = model.model_name || model.model_id;
      if (!id || seen.has(id)) continue;
      seen.add(id);
      models.push({
        id,
        object: 'model',
        created: 0,
        owned_by: provider.name || provider.type || 'custom'
      });
    }
  }
  return models;
}

function buildModelsResponse(cfg, keyCheck = {}, requestedModel = null) {
  const allowedModels = keyCheck.isMaster ? [] : (keyCheck.virtualKey?.allowed_models || []);
  const allowedSet = allowedModels.length > 0 ? new Set(allowedModels) : null;
  const data = getConfiguredModels(cfg).filter(model => !allowedSet || allowedSet.has(model.id));
  if (requestedModel) {
    const found = data.find(model => model.id === requestedModel);
    return found || null;
  }
  return { object: 'list', data };
}

function firstTokenNumber(...values) {
  for (const value of values) {
    if (value === undefined || value === null || value === '') continue;
    const number = Number(value);
    if (Number.isFinite(number)) return number;
  }
  return null;
}

function extractUsageTokens(data, fallbackInput = 0) {
  const usage = data?.usage || {};
  const promptDetails = usage.prompt_tokens_details || {};
  const inputDetails = usage.input_tokens_details || {};
  const cacheHitTokens = firstTokenNumber(
    promptDetails.cached_tokens,
    inputDetails.cached_tokens,
    usage.cached_tokens,
    usage.cache_read_input_tokens,
    usage.cache_read_tokens,
    usage.prompt_cache_hit_tokens
  ) || 0;
  const cacheWriteTokens = firstTokenNumber(
    promptDetails.cache_write_tokens,
    inputDetails.cache_write_tokens,
    usage.cache_write_tokens,
    usage.cache_creation_input_tokens,
    usage.cache_creation_tokens
  ) || 0;
  const cacheMissTokens = firstTokenNumber(usage.prompt_cache_miss_tokens);
  const baseInputTokens = firstTokenNumber(usage.prompt_tokens, usage.input_tokens, usage.total_prompt_tokens);
  let inputTokens;
  if (cacheMissTokens !== null) {
    inputTokens = cacheHitTokens + cacheMissTokens + cacheWriteTokens;
  } else if (usage.prompt_tokens === undefined && usage.input_tokens !== undefined && (usage.cache_read_input_tokens !== undefined || usage.cache_creation_input_tokens !== undefined)) {
    inputTokens = (baseInputTokens || 0) + cacheHitTokens + cacheWriteTokens;
  } else {
    inputTokens = firstTokenNumber(baseInputTokens, usage.total_tokens, fallbackInput) || 0;
  }
  const outputTokens = firstTokenNumber(usage.completion_tokens, usage.output_tokens) || 0;
  return { inputTokens, outputTokens, cachedTokens: cacheHitTokens, cachedWriteTokens: cacheWriteTokens };
}

function getStreamUsageEvent(event) {
  if (event?.usage && typeof event.usage === 'object') return { usage: event.usage };
  if (event?.response?.usage && typeof event.response.usage === 'object') return { usage: event.response.usage };
  if (event?.message?.usage && typeof event.message.usage === 'object') return { usage: event.message.usage };
  return null;
}

function mergeUsageTokens(current, next) {
  return {
    inputTokens: Math.max(current.inputTokens || 0, next.inputTokens || 0),
    outputTokens: Math.max(current.outputTokens || 0, next.outputTokens || 0),
    cachedTokens: Math.max(current.cachedTokens || 0, next.cachedTokens || 0),
    cachedWriteTokens: Math.max(current.cachedWriteTokens || 0, next.cachedWriteTokens || 0)
  };
}

function extractUsageTokensFromStream(streamText, fallbackInput = 0, fallbackOutput = 0) {
  let dataLines = [];
  let mergedUsage = null;

  const flushEvent = () => {
    if (dataLines.length === 0) return;
    const payload = dataLines.join('\n').trim();
    dataLines = [];
    if (!payload || payload === '[DONE]') return;

    let event;
    try { event = JSON.parse(payload); } catch { return; }
    const usageEvent = getStreamUsageEvent(event);
    if (!usageEvent) return;
    const usage = extractUsageTokens(usageEvent, 0);
    mergedUsage = mergedUsage ? mergeUsageTokens(mergedUsage, usage) : usage;
  };

  for (const line of String(streamText || '').split(/\r?\n/)) {
    if (line.trim() === '') {
      flushEvent();
    } else if (line.startsWith('data:')) {
      dataLines.push(line.slice(5).trimStart());
    }
  }
  flushEvent();

  if (!mergedUsage) {
    return { inputTokens: fallbackInput, outputTokens: fallbackOutput, cachedTokens: 0, cachedWriteTokens: 0 };
  }
  return {
    inputTokens: mergedUsage.inputTokens || fallbackInput,
    outputTokens: mergedUsage.outputTokens || fallbackOutput,
    cachedTokens: mergedUsage.cachedTokens || 0,
    cachedWriteTokens: mergedUsage.cachedWriteTokens || 0
  };
}

function calcCost(model, inputTokens, outputTokens, pricingTable, cachedTokens = 0, cachedWriteTokens = 0) {
  const p = pricingTable[model] || { input: 0, cached_input: 0, cached_write: 0, output: 0 };
  const normalInput = Math.max(0, inputTokens - cachedTokens - cachedWriteTokens);
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
  res.json({ success: true, unified: true, message: '配置已保存，统一代理服务会在下一次请求时读取最新配置。' });
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

function getUsageRequestCount(entry) {
  const count = Number(entry?.request_count);
  return Number.isFinite(count) && count > 0 ? count : 1;
}

function isArchivedUsageEntry(entry) {
  return entry?.archived === true;
}

function getUsageArchiveKey(entry) {
  return JSON.stringify([
    entry.model || 'unknown',
    entry.key_name || 'master',
    entry.status || '',
    entry.stream === true,
    entry.error || ''
  ]);
}

function createUsageArchiveEntry(entry, archiveKey) {
  return {
    archived: true,
    request_id: 'archive:' + crypto.createHash('sha1').update(archiveKey).digest('hex').slice(0, 16),
    request_count: 0,
    first_timestamp: 0,
    last_timestamp: 0,
    timestamp: 0,
    model: entry.model || 'unknown',
    key_name: entry.key_name || 'master',
    input_tokens: 0,
    cached_tokens: 0,
    cached_write_tokens: 0,
    output_tokens: 0,
    cost: 0,
    duration_ms: null,
    status: entry.status,
    stream: entry.stream === true,
    user_agent: 'archived',
    error: entry.error || ''
  };
}

function mergeUsageIntoArchive(archiveEntry, entry) {
  const requestCount = getUsageRequestCount(entry);
  const firstTimestamp = Number(entry.first_timestamp || entry.timestamp) || 0;
  const lastTimestamp = Number(entry.last_timestamp || entry.timestamp) || firstTimestamp;
  if (!archiveEntry.first_timestamp || (firstTimestamp && firstTimestamp < archiveEntry.first_timestamp)) archiveEntry.first_timestamp = firstTimestamp;
  if (lastTimestamp >= archiveEntry.last_timestamp) {
    archiveEntry.last_timestamp = lastTimestamp;
    archiveEntry.timestamp = lastTimestamp;
  }

  archiveEntry.request_count += requestCount;
  archiveEntry.input_tokens += entry.input_tokens || 0;
  archiveEntry.cached_tokens += entry.cached_tokens || 0;
  archiveEntry.cached_write_tokens += entry.cached_write_tokens || 0;
  archiveEntry.output_tokens += entry.output_tokens || 0;
  archiveEntry.cost += entry.cost || 0;

  if (Number.isFinite(entry.duration_ms)) {
    archiveEntry._duration_total_ms = (archiveEntry._duration_total_ms || 0) + entry.duration_ms * requestCount;
    archiveEntry._duration_count = (archiveEntry._duration_count || 0) + requestCount;
  }
}

function finalizeUsageArchiveEntry(archiveEntry) {
  if (archiveEntry._duration_count > 0) {
    archiveEntry.duration_ms = Math.round(archiveEntry._duration_total_ms / archiveEntry._duration_count);
  }
  delete archiveEntry._duration_total_ms;
  delete archiveEntry._duration_count;
  return archiveEntry;
}

function archiveUsageEntries(entries, rawLimit = USAGE_RECENT_DETAIL_LIMIT) {
  const normalizedEntries = Array.isArray(entries) ? entries : [];
  const rawCount = normalizedEntries.filter(e => !isArchivedUsageEntry(e)).length;
  const overflowCount = Math.max(0, rawCount - rawLimit);
  if (overflowCount === 0) {
    return { entries: normalizedEntries, changed: false, archivedCount: 0, rawLimit };
  }

  const archives = new Map();
  const rawToKeep = [];
  let rawSeen = 0;

  const mergeArchive = (entry) => {
    const key = getUsageArchiveKey(entry);
    let archiveEntry = archives.get(key);
    if (!archiveEntry) {
      archiveEntry = createUsageArchiveEntry(entry, key);
      archives.set(key, archiveEntry);
    }
    mergeUsageIntoArchive(archiveEntry, entry);
  };

  for (const entry of normalizedEntries) {
    if (isArchivedUsageEntry(entry)) {
      mergeArchive(entry);
      continue;
    }

    if (rawSeen < overflowCount) {
      mergeArchive(entry);
    } else {
      rawToKeep.push(entry);
    }
    rawSeen++;
  }

  const archiveEntries = Array.from(archives.values())
    .map(finalizeUsageArchiveEntry)
    .sort((a, b) => (a.timestamp || 0) - (b.timestamp || 0));
  return { entries: [...archiveEntries, ...rawToKeep], changed: true, archivedCount: overflowCount, rawLimit };
}

function writeUsageIndex() {
  const content = usageIndex.map(entry => JSON.stringify(entry)).join('\n');
  fs.writeFileSync(USAGE_FILE, content ? content + '\n' : '', 'utf8');
}

function archiveUsageIndexIfNeeded() {
  const result = archiveUsageEntries(usageIndex);
  if (!result.changed) return result;
  usageIndex = result.entries;
  try {
    writeUsageIndex();
  } catch (e) {
    console.error('[Usage] Archive rewrite failed:', e.message);
  }
  return result;
}

function computeUsageStats(entries) {
  const stats = {
    totalRequests: 0,
    totalInputTokens: 0,
    totalCachedTokens: 0,
    totalOutputTokens: 0,
    totalCost: 0,
    cacheHitRate: 0,
    byModel: {},
    byKey: {}
  };
  for (const e of entries) {
    const requestCount = getUsageRequestCount(e);
    stats.totalRequests += requestCount;
    stats.totalInputTokens += e.input_tokens || 0;
    stats.totalCachedTokens += e.cached_tokens || 0;
    stats.totalOutputTokens += e.output_tokens || 0;
    stats.totalCost += e.cost || 0;
    if (!stats.byModel[e.model]) stats.byModel[e.model] = { requests: 0, input_tokens: 0, cached_tokens: 0, output_tokens: 0, cost: 0 };
    stats.byModel[e.model].requests += requestCount;
    stats.byModel[e.model].input_tokens += e.input_tokens || 0;
    stats.byModel[e.model].cached_tokens += e.cached_tokens || 0;
    stats.byModel[e.model].output_tokens += e.output_tokens || 0;
    stats.byModel[e.model].cost += e.cost || 0;
    const keyName = e.key_name || 'master';
    if (!stats.byKey[keyName]) stats.byKey[keyName] = { requests: 0, cost: 0 };
    stats.byKey[keyName].requests += requestCount;
    stats.byKey[keyName].cost += e.cost || 0;
  }
  if (stats.totalInputTokens > 0) {
    stats.cacheHitRate = ((stats.totalCachedTokens / stats.totalInputTokens) * 100).toFixed(2);
  }
  stats.byModel = Object.fromEntries(
    Object.entries(stats.byModel).sort(([, a], [, b]) => b.requests - a.requests)
  );
  stats.byKey = Object.fromEntries(
    Object.entries(stats.byKey).sort(([, a], [, b]) => b.requests - a.requests)
  );
  return stats;
}

function limitUsageEntries(entries, limit = 1000) {
  const normalizedEntries = Array.isArray(entries) ? entries : [];
  const visibleCandidates = normalizedEntries.filter(e => !isArchivedUsageEntry(e));
  const visibleEntries = visibleCandidates.slice(-limit);
  const rawTotal = normalizedEntries.reduce((sum, entry) => sum + getUsageRequestCount(entry), 0);
  const hiddenTotal = Math.max(0, rawTotal - visibleEntries.length);
  if (hiddenTotal === 0) {
    return { entries: visibleEntries, limited: false, rawTotal, hiddenTotal, limit };
  }
  return { entries: visibleEntries, limited: true, rawTotal, hiddenTotal, limit };
}

app.get('/api/usage', authMiddleware, (req, res) => {
  const model = req.query.model || null;
  const page = Math.max(1, parseInt(req.query.page) || 1);
  const pageSize = Math.max(1, Math.min(500, parseInt(req.query.pageSize) || 20));
  const entries = model ? usageIndex.filter(u => u.model === model) : usageIndex;
  const stats = computeUsageStats(entries);
  const visible = limitUsageEntries(entries, USAGE_RECENT_DETAIL_LIMIT);
  // Pagination: newest first
  const allEntries = visible.entries.slice().reverse();
  const total = allEntries.length;
  const totalPages = Math.max(1, Math.ceil(total / pageSize));
  const currentPage = Math.min(page, totalPages);
  const offset = (currentPage - 1) * pageSize;
  const pagedEntries = allEntries.slice(offset, offset + pageSize);
  res.json({ entries: pagedEntries, stats, pagination: { total, rawTotal: visible.rawTotal, hiddenTotal: visible.hiddenTotal, limited: visible.limited, limit: visible.limit, page: currentPage, pageSize, totalPages } });
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
    const columns = ['request_id','timestamp','archived','request_count','first_timestamp','last_timestamp','model','key_name','input_tokens','cached_tokens','cached_write_tokens','output_tokens','cost','duration_ms','status','stream','user_agent','error'];
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
  if (MINIMAX_NATIVE_MODELS.has(model)) {
    const minimaxProvider = (cfg.providers || []).find(provider => {
      const baseUrl = String(provider.base_url || '').toLowerCase();
      const name = String(provider.name || '').toLowerCase();
      return baseUrl.includes('minimaxi.com') || name.includes('minimax');
    });
    if (minimaxProvider) return { ...minimaxProvider, model_id: model };
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

function resolveConversationStorageDirectory(cfg) {
  const directory = String(cfg?.conversation_storage?.directory || '').trim();
  if (!directory) throw new Error('conversation storage directory is not configured');
  if (!path.isAbsolute(directory)) throw new Error('conversation storage directory must be absolute');
  return path.resolve(directory);
}

// ---------- Proxy with Logging & Limits ----------

async function proxyRequest(req, res, targetPath) {
  const requestId = crypto.randomUUID();
  const contentType = req.headers['content-type'] || '';
  let parsedBody;
  try {
    parsedBody = parseProxyBody(req.body, contentType);
  } catch (e) {
    return res.status(400).json({ error: { message: '请求体解析失败: ' + e.message, type: 'invalid_request_error', code: 400 } });
  }

  const body = parsedBody.body || {};
  const rawBody = parsedBody.rawBody || Buffer.alloc(0);
  const modelsEndpoint = isModelsEndpoint(targetPath);
  const requestModel = getRequestModel(body, targetPath, rawBody, contentType);
  const model = requestModel || (modelsEndpoint ? 'models' : 'unknown');
  const isStream = body.stream === true || body.stream === 'true';
  const estimatedInput = estimateTokens(body);
  const startTime = Date.now();
  const cfg = loadConfig();

  const authHeader = req.headers['authorization'] || '';
  const authKey = authHeader.startsWith('Bearer ') ? authHeader.slice(7) : '';
  const keyCheck = checkVirtualKey(cfg, authKey, modelsEndpoint ? null : model);
  if (!keyCheck.allowed) {
    return res.status(429).json({ error: { message: keyCheck.reason, type: 'limit_error', code: 429 } });
  }

  if (modelsEndpoint) {
    const modelsResponse = buildModelsResponse(cfg, keyCheck, requestModel);
    if (!modelsResponse) {
      return res.status(404).json({ error: { message: `未知模型: ${requestModel}`, type: 'invalid_request_error', code: 404 } });
    }
    return res.json(modelsResponse);
  }

  if (!modelsEndpoint && !requestModel) {
    return res.status(400).json({ error: { message: '请求体缺少 model 字段，无法路由到兼容 API 服务商', type: 'invalid_request_error', code: 400 } });
  }

  if (!modelsEndpoint) {
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
    const provider = resolveProvider(model, cfg);
    if (!provider) {
      return res.status(400).json({ error: { message: `未知模型: ${model}`, type: 'invalid_request_error', code: 400 } });
    }
    const upstreamUrl = buildUpstreamUrl(provider.base_url, targetPath);
    const upstreamHeaders = {
      'Content-Type': contentType || 'application/json',
      'Accept': req.headers['accept'] || 'application/json',
      'Authorization': `Bearer ${provider.api_key}`
    };
    ['openai-beta', 'anthropic-version', 'anthropic-beta', 'idempotency-key'].forEach(h => {
      if (req.headers[h]) upstreamHeaders[h] = req.headers[h];
    });
    if (provider.user_agent) {
      upstreamHeaders['User-Agent'] = provider.user_agent;
    }
    const upstreamBody = serializeUpstreamBody({
      rawBody,
      body,
      contentType,
      requestModel: model,
      upstreamModel: provider.model_id
    });
    const response = await fetch(upstreamUrl, {
      method: req.method,
      headers: upstreamHeaders,
      body: req.method !== 'GET' && req.method !== 'HEAD' ? upstreamBody : undefined,
      signal: abortController.signal
    });

    if (!isStream) {
      const responseBuffer = Buffer.from(await response.arrayBuffer());
      const responseText = responseBuffer.toString('utf8');
      let data;
      try { data = JSON.parse(responseText); } catch (parseErr) {
        const responseType = response.headers.get('content-type') || 'application/octet-stream';
        const cost = calcCost(model, estimatedInput, 0, cfg.pricing || {}, 0, 0);
        appendUsage({ request_id: requestId, timestamp: Date.now(), model, input_tokens: estimatedInput, cached_tokens: 0, cached_write_tokens: 0, output_tokens: 0, cost, duration_ms: Date.now() - startTime, status: response.status, stream: false, api_key: authKey, key_name: keyName, user_agent: req.headers['user-agent'] || '' });
        res.status(response.status);
        res.setHeader('Content-Type', responseType);
        const disposition = response.headers.get('content-disposition');
        if (disposition) res.setHeader('Content-Disposition', disposition);
        res.send(responseBuffer);
        saveConversation(cfg, { request_id: requestId, timestamp: Date.now(), model, key_name: keyName, user_agent: req.headers['user-agent'] || '', input: body, output: { content_type: responseType, bytes: responseBuffer.length } });
        return;
      }
      const { inputTokens, outputTokens, cachedTokens, cachedWriteTokens } = extractUsageTokens(data, estimatedInput);
      const cost = calcCost(model, inputTokens, outputTokens, cfg.pricing || {}, cachedTokens, cachedWriteTokens);
      appendUsage({ request_id: requestId, timestamp: Date.now(), model, input_tokens: inputTokens, cached_tokens: cachedTokens, cached_write_tokens: cachedWriteTokens, output_tokens: outputTokens, cost, duration_ms: Date.now() - startTime, status: response.status, stream: false, api_key: authKey, key_name: keyName, user_agent: req.headers['user-agent'] || '' });
      res.status(response.status).json(data);
      saveConversation(cfg, { request_id: requestId, timestamp: Date.now(), model, key_name: keyName, user_agent: req.headers['user-agent'] || '', input: body, output: data });
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
      const streamText = streamChunks.join('');
      const estimatedOutput = Math.ceil(outputChunks / 16);
      const { inputTokens, outputTokens, cachedTokens, cachedWriteTokens } = extractUsageTokensFromStream(streamText, estimatedInput, estimatedOutput);
      const cost = calcCost(model, inputTokens, outputTokens, cfg.pricing || {}, cachedTokens, cachedWriteTokens);
      appendUsage({ request_id: requestId, timestamp: Date.now(), model, input_tokens: inputTokens, cached_tokens: cachedTokens, cached_write_tokens: cachedWriteTokens, output_tokens: outputTokens, cost, duration_ms: Date.now() - startTime, status: response.status, stream: true, api_key: authKey, key_name: keyName, user_agent: req.headers['user-agent'] || '' });
      saveConversation(cfg, { request_id: requestId, timestamp: Date.now(), model, key_name: keyName, user_agent: req.headers['user-agent'] || '', input: body, output_raw: streamText });
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

function getProxyTargetPath(req) {
  return req.originalUrl.replace(/^\/proxy(?=\/|$)/, '') || '/';
}

app.all('/proxy/v1/*', (req, res) => proxyRequest(req, res, getProxyTargetPath(req)));
app.all('/proxy/*', (req, res) => proxyRequest(req, res, getProxyTargetPath(req)));
app.all('/v1/*', (req, res) => proxyRequest(req, res, getProxyTargetPath(req)));
app.all(COMPATIBLE_PROXY_ROUTES, (req, res) => proxyRequest(req, res, getProxyTargetPath(req)));

// ---------- Init ----------

function startServer() {
  ensureFiles();
  loadUsageIndex();
  return app.listen(PORT, '0.0.0.0', () => {
    console.log(`LiteLLM Web UI + Proxy running at http://localhost:${PORT}`);
    console.log(`Proxy endpoints: http://localhost:${PORT}/proxy/v1/chat/completions, /proxy/v1/responses, /proxy/v1/embeddings`);
    if (TRAY_ENABLED) initTray();
  });
}

if (require.main === module && process.env.LITELLM_WEBUI_TEST !== '1') {
  startServer();
}

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

app.post('/api/conversation-storage/open', authMiddleware, (req, res) => {
  try {
    const cfg = loadConfig();
    const directory = resolveConversationStorageDirectory(cfg);
    fs.mkdirSync(directory, { recursive: true });
    if (process.platform !== 'win32') {
      return res.status(400).json({ error: 'Opening folders is only supported on Windows in this app' });
    }
    execFile('explorer.exe', [directory], (err) => {
      if (err) return res.status(500).json({ error: err.message });
      res.json({ success: true, directory });
    });
  } catch (e) { res.status(400).json({ error: e.message }); }
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

function quoteCmdArg(value) {
  return `"${String(value).replace(/"/g, '\\"')}"`;
}

function buildTrayRestartCommand(nodePath = process.execPath, serverPath = __filename) {
  return `timeout /t 1 /nobreak >nul & ${quoteCmdArg(nodePath)} ${quoteCmdArg(serverPath)} --tray`;
}

function restartTrayService(systray) {
  const command = buildTrayRestartCommand();
  const child = spawn('cmd.exe', ['/d', '/s', '/c', command], {
    cwd: __dirname,
    detached: true,
    stdio: 'ignore',
    windowsHide: true,
    env: { ...process.env, ENABLE_TRAY: '1' }
  });
  child.unref();
  if (systray) systray.kill();
  setTimeout(() => process.exit(0), 100);
}

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
          { title: '重启服务', tooltip: '重新加载后端代码并重启本地代理', checked: false, enabled: true },
          { title: '退出', tooltip: '关闭所有服务', checked: false, enabled: true }
        ]
      },
      debug: false,
      copyDir: true
    });

    systray.onClick(action => {
      if (action.seq_id === 0) {
        exec(`start http://localhost:${PORT}`);
      } else if (action.seq_id === 1) {
        restartTrayService(systray);
      } else if (action.seq_id === 2) {
        systray.kill();
        process.exit(0);
      }
    });

    // 每30秒更新一次托盘提示，显示总成本和缓存命中率
    setInterval(() => {
      const stats = computeUsageStats(usageIndex);
      const rate = Number(stats.cacheHitRate || 0).toFixed(1);
      const title = `LiteLLM Proxy | 成本: ¥${stats.totalCost.toFixed(2)} | 缓存: ${rate}%`;
      if (typeof systray.setTitle === 'function') systray.setTitle(title);
    }, 30000);
  } catch (e) {
    console.log('[Tray] Systray init failed:', e.message);
  }
}

module.exports = {
  app,
  startServer,
  buildUpstreamUrl,
  getRequestModel,
  resolveProvider,
  MINIMAX_NATIVE_MODELS,
  estimateRequestTokens: estimateTokens,
  computeUsageStats,
  archiveUsageEntries,
  limitUsageEntries,
  extractUsageTokens,
  extractUsageTokensFromStream,
  calcCost,
  resolveConversationStorageDirectory,
  buildTrayRestartCommand,
  parseProxyBody,
  serializeUpstreamBody,
  extractMultipartField,
  replaceMultipartField,
  isModelsEndpoint,
  buildModelsResponse,
  normalizeProxyPath,
  getProxyTargetPath
};
