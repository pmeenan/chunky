// Generic runner for the server-published capture list (capture.json).
// Steps one tab through items in order. Each item loads to completion, is
// inspected for challenge pages (patterns come from the list), optionally
// discovers links to visit next, then the worker waits a random pause (at
// least the item's minDelay) before the next item. Pauses stay under 30 s so
// the service worker never idles out between steps.
//
// Item: { url, minDelay?, discover?: { count, host, path_pattern } }

const state = { running: false, tabId: null, items: [], i: 0, min: 8, max: 20, challenge: null, status: 'idle', waiting: false, timer: null };

const publish = () =>
  chrome.storage.session.set({ state: { running: state.running, i: state.i, total: state.items.length, status: state.status } });

function finish(status) {
  state.running = false;
  state.waiting = false;
  clearTimeout(state.timer);
  state.status = status;
  publish();
}

async function step() {
  if (!state.running) return;
  if (state.i >= state.items.length) return finish(`done: ${state.items.length} pages`);
  state.waiting = true;
  state.status = `loading ${state.i + 1}/${state.items.length}`;
  publish();
  try {
    await chrome.tabs.update(state.tabId, { url: state.items[state.i].url });
  } catch (e) {
    finish(`stopped: ${e.message}`);
  }
}

// Runs in the page: challenge check and candidate links. Text is only judged
// on short pages, so ordinary pages that mention CAPTCHAs never stop a run.
function inspectPage(c) {
  const body = document.body ? document.body.innerText : '';
  const frames = [...document.querySelectorAll('iframe[src], script[src]')].map((e) => e.src);
  const challenge = frames.some((src) => new RegExp(c.frame_pattern, 'i').test(src)) ||
    (body.length <= c.max_text_length && new RegExp(c.text_pattern, 'i').test(document.title + ' ' + body));
  return { challenge, links: [...document.querySelectorAll('a[href]')].map((a) => a.href) };
}

// Same-host links whose path matches the pattern, reduced to origin + path
// (no query strings or fragments), deduplicated, at most `count`.
function discoverLinks(links, rule) {
  const re = new RegExp(rule.path_pattern);
  const out = [];
  for (const h of links) {
    try {
      const u = new URL(h);
      if (u.hostname === rule.host && re.test(u.pathname)) {
        const clean = `${u.origin}${u.pathname}`;
        if (!out.includes(clean)) out.push(clean);
      }
    } catch {}
    if (out.length >= rule.count) break;
  }
  return out;
}

chrome.tabs.onUpdated.addListener(async (tabId, info, tab) => {
  if (!state.running || !state.waiting || tabId !== state.tabId || info.status !== 'complete') return;
  if (!tab.url || tab.url === 'about:blank') return;
  state.waiting = false;
  const item = state.items[state.i];
  const where = `${state.i + 1}/${state.items.length}`;
  if (new RegExp(state.challenge.url_pattern, 'i').test(tab.url)) {
    return finish(`stopped: challenge page at ${where}; capture is complete up to the previous page`);
  }
  let page;
  try {
    [{ result: page }] = await chrome.scripting.executeScript({ target: { tabId }, func: inspectPage, args: [state.challenge] });
  } catch (e) {
    return finish(`stopped: could not inspect page ${where} (${e.message})`);
  }
  if (page.challenge) return finish(`stopped: challenge or block page at ${where}; capture is complete up to the previous page`);
  if (item.discover) {
    const found = discoverLinks(page.links, item.discover).map((url) => ({ url, minDelay: item.minDelay }));
    state.items.splice(state.i + 1, 0, ...found);
  }
  state.i++;
  if (state.i >= state.items.length) return finish(`done: ${state.items.length} pages`);
  const next = state.items[state.i];
  const lo = Math.min(25, Math.max(state.min, next.minDelay || 0));
  const hi = Math.min(25, Math.max(lo, state.max));
  const ms = (lo + Math.random() * (hi - lo)) * 1000;
  state.status = `loaded ${state.i}/${state.items.length}; next in ${Math.round(ms / 1000)} s`;
  publish();
  state.timer = setTimeout(step, ms);
});

chrome.tabs.onRemoved.addListener((tabId) => {
  if (state.running && tabId === state.tabId) finish('stopped: tab closed');
});

chrome.runtime.onMessage.addListener((msg, _sender, reply) => {
  if (msg.cmd === 'start') {
    clearTimeout(state.timer);
    Object.assign(state, {
      running: true,
      tabId: msg.tabId,
      items: msg.items,
      i: 0,
      min: Math.max(3, msg.min),
      max: Math.min(25, Math.max(msg.min, msg.max)),
      challenge: msg.challenge,
    });
    step();
  } else if (msg.cmd === 'stop') {
    finish(`stopped by user at ${state.i}/${state.items.length}`);
  }
  reply({ ok: true });
});
