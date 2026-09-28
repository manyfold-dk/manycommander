// manycommander.app: www redirects to the apex; everything else is a static asset from
// public/ (the Zola build). Cloudflare does not apply static/_headers to responses a Worker
// returns, and with run_worker_first every response is one, so this Worker applies it.
import headersFile from "./static/_headers";

const APEX = "manycommander.app";
const RULES = parseHeaders(headersFile);

export default {
  async fetch(request, env) {
    const url = new URL(request.url);
    const path = url.pathname;
    let response;
    if (url.hostname === `www.${APEX}`) {
      url.protocol = "https:";
      url.hostname = APEX;
      url.port = "";
      response = new Response(null, { status: 301, headers: { Location: url.href } });
    } else {
      response = await env.ASSETS.fetch(request);
    }
    return withHeaders(response, path);
  },
};

// The _headers format: a path pattern on its own line, then indented "Name: value" lines.
// A pattern ending in `*` matches every path with that prefix.
function parseHeaders(text) {
  const rules = [];
  let rule = null;
  for (const line of text.split(/\r?\n/)) {
    if (!line.trim() || line.trim().startsWith("#")) continue;
    if (/^\s/.test(line)) {
      const colon = line.indexOf(":");
      if (!rule || colon < 0) throw new Error(`_headers: cannot parse "${line.trim()}"`);
      rule.headers.push([line.slice(0, colon).trim(), line.slice(colon + 1).trim()]);
    } else {
      rule = { pattern: line.trim(), headers: [] };
      rules.push(rule);
    }
  }
  return rules;
}

function withHeaders(response, path) {
  const out = new Response(response.body, response);
  for (const { pattern, headers } of RULES) {
    const hit = pattern.endsWith("*") ? path.startsWith(pattern.slice(0, -1)) : path === pattern;
    if (hit) for (const [name, value] of headers) out.headers.set(name, value);
  }
  return out;
}
