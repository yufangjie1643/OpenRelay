const assert = require('assert');

process.env.OPENRELAY_TEST = '1';
const proxy = require('./server');

assert.strictEqual(typeof proxy.buildUpstreamUrl, 'function');
assert.strictEqual(typeof proxy.getRequestModel, 'function');
assert.strictEqual(typeof proxy.estimateRequestTokens, 'function');
assert.strictEqual(typeof proxy.extractUsageTokens, 'function');
assert.strictEqual(typeof proxy.extractUsageTokensFromStream, 'function');
assert.strictEqual(typeof proxy.calcCost, 'function');
assert.strictEqual(typeof proxy.resolveConversationStorageDirectory, 'function');
assert.strictEqual(typeof proxy.buildTrayRestartCommand, 'function');
assert.strictEqual(typeof proxy.computeUsageStats, 'function');
assert.strictEqual(typeof proxy.isModelsEndpoint, 'function');
assert.strictEqual(typeof proxy.buildModelsResponse, 'function');
assert.strictEqual(typeof proxy.serializeUpstreamBody, 'function');

assert.strictEqual(
  proxy.buildUpstreamUrl('https://api.example.com/v1', '/v1/chat/completions', '?timeout=30'),
  'https://api.example.com/v1/chat/completions?timeout=30'
);
assert.strictEqual(
  proxy.buildUpstreamUrl('https://api.example.com', '/v1/embeddings', ''),
  'https://api.example.com/v1/embeddings'
);
assert.strictEqual(
  proxy.buildUpstreamUrl('https://api.example.com/openai/v1/', '/responses', '?beta=1'),
  'https://api.example.com/openai/v1/responses?beta=1'
);

assert.strictEqual(proxy.getRequestModel({ model: 'gpt-4o-mini' }, '/v1/chat/completions'), 'gpt-4o-mini');
assert.strictEqual(proxy.getRequestModel({ model: 'text-embedding-3-small' }, '/v1/embeddings'), 'text-embedding-3-small');
assert.strictEqual(proxy.getRequestModel({ model: 'speech-2.8-hd' }, '/v1/t2a_v2'), 'speech-2.8-hd');
assert.strictEqual(proxy.getRequestModel({ model: 'image-01-live' }, '/v1/image_generation'), 'image-01-live');
assert.strictEqual(proxy.getRequestModel({ model: 'MiniMax-Hailuo-2.3' }, '/v1/video_generation'), 'MiniMax-Hailuo-2.3');
assert.strictEqual(proxy.getRequestModel({ model: 'music-2.6' }, '/v1/music_generation'), 'music-2.6');
assert.strictEqual(proxy.getRequestModel({}, '/v1/models/gpt-4o-mini'), 'gpt-4o-mini');
assert.strictEqual(proxy.getRequestModel({}, '/v1/models'), null);
assert.strictEqual(proxy.getProxyTargetPath({ originalUrl: '/v1/chat/completions?stream=false' }), '/v1/chat/completions?stream=false');
assert.strictEqual(proxy.getProxyTargetPath({ originalUrl: '/proxy/v1/responses?trace=1' }), '/v1/responses?trace=1');
assert.strictEqual(
  proxy.resolveProvider('image-01', { providers: [{ name: 'MiniMax', base_url: 'https://api.minimaxi.com/v1', models: [] }] }).model_id,
  'image-01'
);
assert.strictEqual(
  proxy.resolveProvider('unknown-model', { providers: [{ name: 'MiniMax', base_url: 'https://api.minimaxi.com/v1', models: [] }] }),
  null
);

assert.strictEqual(proxy.isModelsEndpoint('/v1/models'), true);
assert.strictEqual(proxy.isModelsEndpoint('/models/gpt-4o-mini'), true);
assert.strictEqual(proxy.isModelsEndpoint('/v1/chat/completions'), false);

const modelsResponse = proxy.buildModelsResponse({
  providers: [
    { name: 'MiniMax', models: [{ model_name: 'MiniMax-M2.7' }, { model_name: 'MiniMax-M2.5' }] },
    { name: 'DeepSeek', models: [{ model_name: 'deepseek-v4-flash' }] }
  ]
}, { isMaster: false, virtualKey: { allowed_models: ['MiniMax-M2.7', 'deepseek-v4-flash'] } });
assert.deepStrictEqual(
  modelsResponse.data.map(m => m.id),
  ['MiniMax-M2.7', 'deepseek-v4-flash'],
  'models endpoint should return locally configured models filtered by virtual key permissions'
);
assert.strictEqual(modelsResponse.object, 'list');

assert(proxy.estimateRequestTokens({ messages: [{ content: 'hello world' }] }) > 0);
assert(proxy.estimateRequestTokens({ input: ['alpha beta', 'gamma'] }) > 0);
assert(proxy.estimateRequestTokens({ prompt: 'complete this text' }) > 0);
assert(proxy.estimateRequestTokens({ text: 'hello speech', prompt: 'music prompt', lyrics: 'line one' }) > 0);

const multipart = Buffer.from(
  '--abc\r\nContent-Disposition: form-data; name="model"\r\n\r\nspeech-2.8-hd\r\n--abc\r\nContent-Disposition: form-data; name="file"; filename="a.wav"\r\n\r\nxxx\r\n--abc--\r\n'
);
assert.strictEqual(proxy.extractMultipartField(multipart, 'multipart/form-data; boundary=abc', 'model'), 'speech-2.8-hd');
assert.strictEqual(
  proxy.extractMultipartField(proxy.replaceMultipartField(multipart, 'multipart/form-data; boundary=abc', 'model', 'speech-2.8-turbo'), 'multipart/form-data; boundary=abc', 'model'),
  'speech-2.8-turbo'
);
assert.strictEqual(proxy.parseProxyBody(Buffer.from('{"model":"image-01","prompt":"draw"}'), 'application/json').body.model, 'image-01');
assert.strictEqual(proxy.parseProxyBody(Buffer.from('model=music-2.6&prompt=lofi'), 'application/x-www-form-urlencoded').body.model, 'music-2.6');
assert.deepStrictEqual(
  JSON.parse(proxy.serializeUpstreamBody({
    rawBody: Buffer.from(JSON.stringify({
      model: 'local-search-model',
      input: 'latest news',
      tools: [{ type: 'web_search_preview' }],
      web_search_options: { search_context_size: 'high' }
    })),
    body: {
      model: 'local-search-model',
      input: 'latest news',
      tools: [{ type: 'web_search_preview' }],
      web_search_options: { search_context_size: 'high' }
    },
    contentType: 'application/json',
    requestModel: 'local-search-model',
    upstreamModel: 'provider-search-model'
  })),
  {
    model: 'provider-search-model',
    input: 'latest news',
    tools: [{ type: 'web_search_preview' }],
    web_search_options: { search_context_size: 'high' }
  },
  'provider-native web search fields should be forwarded unchanged while model aliases are mapped'
);
assert.deepStrictEqual(
  JSON.parse(proxy.serializeUpstreamBody({
    rawBody: Buffer.from(JSON.stringify({
      model: 'claude-local',
      messages: [{ role: 'user', content: 'search the web' }],
      tools: [{ type: 'web_search_20250305', name: 'web_search', max_uses: 3 }]
    })),
    body: {
      model: 'claude-local',
      messages: [{ role: 'user', content: 'search the web' }],
      tools: [{ type: 'web_search_20250305', name: 'web_search', max_uses: 3 }]
    },
    contentType: 'application/json',
    requestModel: 'claude-local',
    upstreamModel: 'claude-3-7-sonnet-latest'
  })).tools,
  [{ type: 'web_search_20250305', name: 'web_search', max_uses: 3 }],
  'Anthropic native web_search tools should pass through'
);

assert.deepStrictEqual(
  proxy.extractUsageTokens({ usage: { prompt_tokens: 11, completion_tokens: 7, prompt_tokens_details: { cached_tokens: 3, cache_write_tokens: 2 } } }, 5),
  { inputTokens: 11, outputTokens: 7, cachedTokens: 3, cachedWriteTokens: 2 }
);
assert.deepStrictEqual(
  proxy.extractUsageTokens({ usage: { input_tokens: 13, output_tokens: 8 } }, 5),
  { inputTokens: 13, outputTokens: 8, cachedTokens: 0, cachedWriteTokens: 0 }
);
assert.deepStrictEqual(
  proxy.extractUsageTokens({ usage: { total_tokens: 21 } }, 5),
  { inputTokens: 21, outputTokens: 0, cachedTokens: 0, cachedWriteTokens: 0 }
);
assert.deepStrictEqual(
  proxy.extractUsageTokens({ usage: { prompt_tokens: 100, completion_tokens: 5, cached_tokens: 80 } }, 5),
  { inputTokens: 100, outputTokens: 5, cachedTokens: 80, cachedWriteTokens: 0 }
);
assert.deepStrictEqual(
  proxy.extractUsageTokens({ usage: { prompt_cache_hit_tokens: 80, prompt_cache_miss_tokens: 20, completion_tokens: 5 } }, 5),
  { inputTokens: 100, outputTokens: 5, cachedTokens: 80, cachedWriteTokens: 0 }
);
assert.deepStrictEqual(
  proxy.extractUsageTokensFromStream(
    [
      'data: {"choices":[{"delta":{"content":"ok"}}]}',
      '',
      'data: {"choices":[],"usage":{"prompt_tokens":100,"completion_tokens":7,"cached_tokens":80,"prompt_tokens_details":{"cached_tokens":80}}}',
      '',
      'data: [DONE]',
      ''
    ].join('\n'),
    12,
    3
  ),
  { inputTokens: 100, outputTokens: 7, cachedTokens: 80, cachedWriteTokens: 0 }
);
assert.deepStrictEqual(
  proxy.extractUsageTokensFromStream(
    [
      'event: response.completed',
      'data: {"type":"response.completed","response":{"usage":{"input_tokens":100,"output_tokens":7,"input_tokens_details":{"cached_tokens":80}}}}',
      ''
    ].join('\n'),
    12,
    3
  ),
  { inputTokens: 100, outputTokens: 7, cachedTokens: 80, cachedWriteTokens: 0 }
);
assert.deepStrictEqual(
  proxy.extractUsageTokensFromStream(
    [
      'event: message_start',
      'data: {"type":"message_start","message":{"usage":{"input_tokens":20,"cache_read_input_tokens":80,"cache_creation_input_tokens":10,"output_tokens":1}}}',
      '',
      'event: message_delta',
      'data: {"type":"message_delta","usage":{"output_tokens":7}}',
      ''
    ].join('\n'),
    12,
    3
  ),
  { inputTokens: 110, outputTokens: 7, cachedTokens: 80, cachedWriteTokens: 10 }
);
assert.deepStrictEqual(
  proxy.extractUsageTokensFromStream('data: {"choices":[{"delta":{"content":"ok"}}]}\n\n', 12, 3),
  { inputTokens: 12, outputTokens: 3, cachedTokens: 0, cachedWriteTokens: 0 }
);
assert(
  Math.abs(proxy.calcCost('m', 100, 5, { m: { input: 10, cached_input: 2, cached_write: 4, output: 20 } }, 30, 10) - 0.0008) < 1e-12,
  'cache write tokens should be priced at cached_write rate, not also as normal input'
);
assert.strictEqual(
  proxy.resolveConversationStorageDirectory({ conversation_storage: { directory: 'D:\\little\\litellm-proxy\\conversations' } }),
  'D:\\little\\litellm-proxy\\conversations'
);
assert.throws(
  () => proxy.resolveConversationStorageDirectory({ conversation_storage: { directory: 'conversations' } }),
  /absolute/
);
assert.throws(
  () => proxy.resolveConversationStorageDirectory({ conversation_storage: { directory: '' } }),
  /not configured/
);
assert.strictEqual(
  proxy.buildTrayRestartCommand('C:\\Program Files\\nodejs\\node.exe', 'D:\\little\\litellm-proxy\\web\\server.js'),
  'timeout /t 1 /nobreak >nul & "C:\\Program Files\\nodejs\\node.exe" "D:\\little\\litellm-proxy\\web\\server.js" --tray'
);
{
  const entries = Array.from({ length: 10001 }, (_, i) => ({
    model: i === 0 ? 'expensive-old-model' : 'cheap-new-model',
    key_name: 'test',
    input_tokens: 1,
    cached_tokens: 0,
    output_tokens: 1,
    cost: i === 0 ? 35 : 0
  }));
  const stats = proxy.computeUsageStats(entries);
  assert.strictEqual(stats.totalRequests, 10001);
  assert.strictEqual(stats.totalCost, 35);
  assert.strictEqual(stats.byModel['expensive-old-model'].cost, 35);
}
{
  const stats = proxy.computeUsageStats([
    { archived: true, request_count: 12, model: 'archived-model', key_name: 'old-key', input_tokens: 120, cached_tokens: 60, output_tokens: 30, cost: 9 },
    { model: 'archived-model', key_name: 'old-key', input_tokens: 10, cached_tokens: 5, output_tokens: 2, cost: 1 }
  ]);
  assert.strictEqual(stats.totalRequests, 13);
  assert.strictEqual(stats.byModel['archived-model'].requests, 13);
  assert.strictEqual(stats.byKey['old-key'].requests, 13);
  assert.strictEqual(stats.totalCost, 10);
}
{
  const stats = proxy.computeUsageStats([
    { model: 'low-volume', key_name: 'k', cost: 0 },
    { model: 'high-volume', key_name: 'k', request_count: 5, archived: true, cost: 0 },
    { model: 'medium-volume', key_name: 'k', request_count: 3, archived: true, cost: 0 }
  ]);
  assert.deepStrictEqual(
    Object.keys(stats.byModel),
    ['high-volume', 'medium-volume', 'low-volume'],
    'model usage stats should be sorted by request count descending'
  );
}
assert.strictEqual(typeof proxy.archiveUsageEntries, 'function');
{
  const entries = Array.from({ length: 1002 }, (_, i) => ({
    request_id: `req-${i}`,
    timestamp: 1000 + i,
    model: 'same-model',
    key_name: 'test-key',
    input_tokens: 10,
    cached_tokens: 3,
    cached_write_tokens: 1,
    output_tokens: 4,
    cost: 0.5,
    duration_ms: 20,
    status: 200,
    stream: false,
    user_agent: `ua-${i}`
  }));
  const result = proxy.archiveUsageEntries(entries, 1000);
  assert.strictEqual(result.changed, true);
  assert.strictEqual(result.archivedCount, 2);
  assert.strictEqual(result.entries.length, 1001);
  const archiveRow = result.entries.find(e => e.archived);
  assert(archiveRow, 'archive row should be written for overflow history');
  assert.strictEqual(archiveRow.request_count, 2);
  assert.strictEqual(archiveRow.input_tokens, 20);
  assert.strictEqual(archiveRow.cached_tokens, 6);
  assert.strictEqual(archiveRow.cached_write_tokens, 2);
  assert.strictEqual(archiveRow.output_tokens, 8);
  assert.strictEqual(archiveRow.cost, 1);
  assert.strictEqual(result.entries.filter(e => !e.archived).length, 1000);
  assert.strictEqual(result.entries.filter(e => !e.archived)[0].request_id, 'req-2');
}
assert.strictEqual(typeof proxy.limitUsageEntries, 'function');
{
  const rawEntries = Array.from({ length: 1000 }, (_, i) => ({
    request_id: `req-${i}`,
    timestamp: 1000 + i,
    model: 'same-model',
    key_name: 'test-key',
    input_tokens: 10,
    cached_tokens: 3,
    cached_write_tokens: 1,
    output_tokens: 4,
    cost: 0.5,
    duration_ms: i < 1000 ? 20 : 40,
    status: 200,
    stream: i % 2 === 0,
    user_agent: `ua-${i}`
  }));
  const entries = [
    { archived: true, request_count: 7, model: 'same-model', key_name: 'test-key', input_tokens: 70, cached_tokens: 21, output_tokens: 28, cost: 3.5 },
    ...rawEntries
  ];
  const result = proxy.limitUsageEntries(entries, 1000);
  assert.strictEqual(result.limited, true);
  assert.strictEqual(result.rawTotal, 1007);
  assert.strictEqual(result.hiddenTotal, 7);
  assert.strictEqual(result.entries.length, 1000);
  assert.strictEqual(result.entries[0].request_id, 'req-0');
  assert.strictEqual(result.entries[999].request_id, 'req-999');
  assert.strictEqual(result.entries[0].request_count, undefined);
}

const routePaths = proxy.app._router.stack
  .filter(layer => layer.route)
  .flatMap(layer => Array.isArray(layer.route.path) ? layer.route.path : [layer.route.path]);
assert(routePaths.includes('/v1/*'), 'server should accept OpenAI-compatible /v1/* requests without /proxy prefix');
assert(routePaths.some(routePath => Array.isArray(routePath) || String(routePath).includes('/chat/completions')), 'server should accept common no-prefix compatible endpoints');
assert(routePaths.some(routePath => String(routePath).includes('/t2a_v2')), 'server should accept MiniMax native speech endpoint');
assert(routePaths.some(routePath => String(routePath).includes('/image_generation')), 'server should accept MiniMax native image endpoint');
assert(routePaths.some(routePath => String(routePath).includes('/video_generation')), 'server should accept MiniMax native video endpoint');
assert(routePaths.some(routePath => String(routePath).includes('/music_generation')), 'server should accept MiniMax native music endpoint');
assert(routePaths.some(routePath => String(routePath).includes('/messages')), 'server should accept provider-native messages endpoints for built-in web search tools');

console.log('proxy compatibility checks passed');
