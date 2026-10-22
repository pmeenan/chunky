// Loads the server-published capture list and hands the selected items to the
// background runner. Everything about what to visit lives in capture.json;
// this popup only chooses groups, adds fresh queries and sets pauses.
const LIST_URL = 'https://chunky.meenan.dev/capture/capture.json';
const $ = (id) => document.getElementById(id);
const lines = (t) => t.split('\n').map((l) => l.trim()).filter((l) => l && !l.startsWith('#'));
let list = null;

function selectedItems() {
  const items = [];
  for (const g of list.groups) {
    if ($(`g-${g.name}`).checked) {
      for (const it of g.items) items.push({ url: it.url, minDelay: g.min_delay || 0, discover: it.discover });
    }
    if (list.fresh && list.fresh.after_group === g.name) {
      for (const q of lines($('fresh').value)) items.push({ url: list.fresh.url_template.replace('{q}', encodeURIComponent(q)) });
    }
  }
  return items;
}

function updateCount() {
  if (!list) return;
  const items = selectedItems();
  const discovered = items.reduce((n, it) => n + (it.discover ? it.discover.count : 0), 0);
  $('count').textContent = `${items.length} pages${discovered ? ` + up to ${discovered} discovered` : ''}`;
}

function render() {
  $('ver').textContent = `list ${list.version}`;
  const box = $('groups');
  box.replaceChildren();
  for (const g of list.groups) {
    const label = document.createElement('label');
    const cb = document.createElement('input');
    cb.type = 'checkbox';
    cb.id = `g-${g.name}`;
    cb.checked = g.default !== false;
    cb.addEventListener('change', updateCount);
    label.append(cb, ` ${g.label} (${g.items.length})`);
    box.append(label);
  }
  if (list.fresh) $('freshlabel').textContent = `${list.fresh.label} (one per line)`;
  if (!$('min').value) $('min').value = list.pause.min;
  if (!$('max').value) $('max').value = list.pause.max;
  updateCount();
}

async function load() {
  const saved = await chrome.storage.local.get(['fresh', 'min', 'max', 'list']);
  if (saved.fresh) $('fresh').value = saved.fresh;
  if (saved.min) $('min').value = saved.min;
  if (saved.max) $('max').value = saved.max;
  try {
    const r = await fetch(LIST_URL, { cache: 'no-store' });
    if (!r.ok) throw new Error(`HTTP ${r.status}`);
    list = await r.json();
    chrome.storage.local.set({ list });
  } catch (e) {
    list = saved.list || null;
    $('status').textContent = list ? `could not load the list (${e.message}); using the cached copy` : `could not load the list (${e.message})`;
  }
  if (list) render();
  const { state } = await chrome.storage.session.get('state');
  if (state) $('status').textContent = state.status;
}

for (const id of ['fresh', 'min', 'max']) {
  $(id).addEventListener('input', () => {
    chrome.storage.local.set({ [id]: $(id).value });
    updateCount();
  });
}

$('start').addEventListener('click', async () => {
  if (!list) return;
  const items = selectedItems();
  if (!items.length) return;
  const [tab] = await chrome.tabs.query({ active: true, currentWindow: true });
  await chrome.runtime.sendMessage({ cmd: 'start', tabId: tab.id, items, challenge: list.challenge, min: Number($('min').value), max: Number($('max').value) });
});

$('stop').addEventListener('click', () => chrome.runtime.sendMessage({ cmd: 'stop' }));

chrome.storage.session.onChanged.addListener((changes) => {
  if (changes.state) $('status').textContent = changes.state.newValue.status;
});

load();
