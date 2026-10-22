const LIST_URL = 'https://chunky.meenan.dev/capture/queries.json';
const $ = (id) => document.getElementById(id);
const lines = (t) => t.split('\n').map((l) => l.trim()).filter((l) => l && !l.startsWith('#'));

let etsy = { categories: [], listings_per_category: 0, min_delay: 10 };

function itemsFor(queries, withEtsy) {
  const items = [{ url: 'https://www.google.com/' }, ...queries.map((q) => ({ url: `https://www.google.com/search?q=${encodeURIComponent(q)}` }))];
  if (withEtsy) {
    for (const url of etsy.categories) items.push({ url, minDelay: etsy.min_delay, discover: etsy.listings_per_category });
  }
  return items;
}

function updateCount() {
  $('count').textContent = `${lines($('panel').value).length + lines($('fresh').value).length} queries`;
}

async function load() {
  const saved = await chrome.storage.local.get(['panel', 'fresh', 'min', 'max', 'etsyList']);
  if (saved.fresh) $('fresh').value = saved.fresh;
  if (saved.min) $('min').value = saved.min;
  if (saved.max) $('max').value = saved.max;
  try {
    const r = await fetch(LIST_URL, { cache: 'no-store' });
    const j = await r.json();
    $('panel').value = j.panel.join('\n');
    if (j.etsy) etsy = j.etsy;
    chrome.storage.local.set({ etsyList: etsy });
  } catch {
    $('panel').value = saved.panel || '';
    if (saved.etsyList) etsy = saved.etsyList;
    $('status').textContent = 'could not load the panel from chunky.meenan.dev; using the last saved copy';
  }
  $('etsyinfo').textContent = `${etsy.categories.length} categories × ${etsy.listings_per_category} listings`;
  updateCount();
  const { state } = await chrome.storage.session.get('state');
  if (state) $('status').textContent = state.status;
}

for (const id of ['panel', 'fresh', 'min', 'max']) {
  $(id).addEventListener('input', () => {
    chrome.storage.local.set({ [id]: $(id).value });
    updateCount();
  });
}

$('start').addEventListener('click', async () => {
  const [tab] = await chrome.tabs.query({ active: true, currentWindow: true });
  const queries = [...lines($('panel').value), ...lines($('fresh').value)];
  if (!queries.length) return;
  const items = itemsFor(queries, $('etsy').checked);
  await chrome.runtime.sendMessage({ cmd: 'start', tabId: tab.id, items, min: Number($('min').value), max: Number($('max').value) });
});

$('stop').addEventListener('click', () => chrome.runtime.sendMessage({ cmd: 'stop' }));

chrome.storage.session.onChanged.addListener((changes) => {
  if (changes.state) $('status').textContent = changes.state.newValue.status;
});

load();
