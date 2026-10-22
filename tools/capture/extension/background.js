// Steps one tab through a list of items. Each item loads to completion, then
// the worker waits a random min..max seconds (at least `minDelay` for that
// item) before the next. An Etsy category item can discover a few listing
// links, which run next. Any challenge page stops the run. Delays stay under
// 30 s so the service worker never idles out between steps.
//
// Item: { url, minDelay?, discover? } where discover = number of listings.

const state = { running: false, tabId: null, items: [], i: 0, min: 8, max: 20, status: 'idle', waiting: false, timer: null };

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

// Runs in the page: challenge detection and (for Etsy categories) listing links.
function inspectPage() {
  const text = (document.title + ' ' + (document.body ? document.body.innerText.slice(0, 4000) : '')).toLowerCase();
  const challenge = /unusual traffic|not a robot|captcha|access is temporarily restricted|blocked|verify you are human/.test(text) ||
    !!document.querySelector('iframe[src*="captcha"]');
  const links = [...document.querySelectorAll('a[href*="/listing/"]')].map((a) => a.href);
  return { challenge, links };
}

// Plain /listing/<id>/<slug> paths only: no query strings (robots.txt
// disallows several parameters) and no /stubs/.
function listingUrls(links, n) {
  const out = [];
  for (const h of links) {
    try {
      const u = new URL(h);
      const m = u.pathname.match(/^\/listing\/\d+\/[^/]+$/);
      if (u.hostname === 'www.etsy.com' && m) {
        const clean = `https://www.etsy.com${u.pathname}`;
        if (!out.includes(clean)) out.push(clean);
      }
    } catch {}
    if (out.length >= n) break;
  }
  return out;
}

chrome.tabs.onUpdated.addListener(async (tabId, info, tab) => {
  if (!state.running || !state.waiting || tabId !== state.tabId || info.status !== 'complete') return;
  if (!tab.url || tab.url === 'about:blank') return;
  state.waiting = false;
  const item = state.items[state.i];
  if (/\/sorry\/|captcha|challenge/i.test(tab.url)) {
    return finish(`stopped: challenge page at ${state.i + 1}/${state.items.length}; capture is complete up to the previous page`);
  }
  if (/^https:\/\/www\.etsy\.com\//.test(tab.url)) {
    let page;
    try {
      [{ result: page }] = await chrome.scripting.executeScript({ target: { tabId }, func: inspectPage });
    } catch (e) {
      return finish(`stopped: could not inspect Etsy page (${e.message})`);
    }
    if (page.challenge) return finish(`stopped: Etsy challenge or block at ${state.i + 1}/${state.items.length}`);
    if (item.discover) {
      const found = listingUrls(page.links, item.discover).map((url) => ({ url, minDelay: item.minDelay }));
      state.items.splice(state.i + 1, 0, ...found);
    }
  }
  state.i++;
  if (state.i >= state.items.length) return finish(`done: ${state.items.length} pages`);
  const next = state.items[state.i];
  const lo = Math.max(state.min, next.minDelay || 0);
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
    });
    step();
  } else if (msg.cmd === 'stop') {
    finish(`stopped by user at ${state.i}/${state.items.length}`);
  }
  reply({ ok: true });
});
