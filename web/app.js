"use strict";

const $ = (id) => document.getElementById(id);
const regions = {
  unknown: "Unknown", custom: "Custom", world_mix: "World Mix", europe: "Europe", asia: "Asia",
  northern_america: "Northern America", latin_america_and_caribbean: "Latin America and the Caribbean",
  africa: "Africa", oceania: "Oceania",
};
const countryNames = new Intl.DisplayNames(["en"], { type: "region" });
const messages = {
  unauthorized: "Сессия завершена. Войдите снова.", invalid_login: "Неверный логин или пароль.",
  rate_limited: "Слишком много попыток входа. Повторите через минуту.",
  internal_error: "Не удалось выполнить запрос. Попробуйте ещё раз.",
  invalid_format: "Строка не соответствует выбранному формату.",
  ambiguous_format: "Неоднозначная строка — выберите формат вручную.",
  invalid_port: "Порт должен быть целым числом от 1 до 65535.", invalid_host: "Некорректный IP-адрес или хост.",
  invalid_credentials: "Некорректные или слишком длинные логин и пароль.",
  invalid_encoding: "Некорректное кодирование логина или пароля в URL.",
  unsupported_protocol: "Поддерживаются HTTP, HTTPS, SOCKS4 и SOCKS5.",
  protocol_mismatch: "Протокол не соответствует выбранному формату.",
  import_too_large: "Максимальный размер списка — 2 МБ.", too_many_lines: "Не более 10 000 строк за один импорт.",
  line_too_long: "Строка длиннее 4096 байт.", empty_import: "Добавьте хотя бы один прокси.",
  invalid_lines: "Исправьте ошибки или включите пропуск ошибочных строк.", no_valid_proxies: "Нет корректных прокси для добавления.",
  incompatible_format: "Этот формат не сохраняет реквизиты без изменений. Выберите URL или CSV.",
  invalid_country: "Выберите страну из списка или Unknown.", invalid_filter: "Некорректный фильтр или номер страницы.",
  invalid_expires_at: "Укажите корректные дату и время окончания срока действия.",
  invalid_key_name: "Укажите название ключа длиной до 100 байт.", too_many_keys: "Достигнут лимит 20 ключей. Отзовите ненужный ключ.",
  export_too_large: "Выгрузка ограничена 50 000 прокси. Уточните фильтры.",
  not_found: "Запись не найдена.", invalid_origin: "Адрес страницы не совпадает с адресом сервиса в настройках APP_ORIGIN.",
  csrf_required: "Обновите страницу и повторите действие.",
};
let page = 1, items = [], total = 0, selected = new Set(), listVersion = 0, previewVersion = 0;
let previewData = null, countriesLoaded = false, toastTimer;

function countryName(code) { return code === "unknown" ? "Unknown" : `${countryNames.of(code)} (${code})`; }
function setError(id, error) { $(id).textContent = error?.message || ""; $(id).hidden = !error; }
function notify(message) {
  $("toast").textContent = message; $("toast").hidden = false;
  clearTimeout(toastTimer); toastTimer = setTimeout(() => { $("toast").hidden = true; }, 4500);
}
function endpoint(item) { return `${item.host.includes(":") ? `[${item.host}]` : item.host}:${item.port}`; }
function cell(text, className = "") { const el = document.createElement("td"); el.textContent = text; el.className = className; return el; }
function expiryValue(id) {
  const input = $(id);
  if (!input.checkValidity()) throw new Error(messages.invalid_expires_at);
  if (!input.value) return null;
  const date = new Date(input.value);
  if (!Number.isFinite(date.getTime())) throw new Error(messages.invalid_expires_at);
  return date.toISOString();
}
function localDateTime(value) {
  if (!value) return "";
  const date = new Date(value);
  return new Date(date.getTime() - date.getTimezoneOffset() * 60000).toISOString().slice(0, 19);
}
function expiryLabel(value) { return value ? new Date(value).toLocaleString("ru-RU", { dateStyle: "short", timeStyle: "short" }) : "Без ограничения"; }
for (const element of document.querySelectorAll("[data-local-timezone]")) element.textContent = Intl.DateTimeFormat().resolvedOptions().timeZone;
function showLogin() {
  ++listVersion; ++previewVersion;
  document.querySelectorAll("dialog[open]").forEach((dialog) => dialog.close());
  $("app-view").hidden = true; $("loading").hidden = true; $("login-view").hidden = false;
  $("proxy-rows").replaceChildren(); items = []; selected.clear();
  $("login-form").elements.password.value = "";
}
async function api(path, { method = "GET", data, raw = false } = {}) {
  let response;
  try {
    response = await fetch(path, { method, credentials: "same-origin", cache: "no-store", headers: {
      ...(method !== "GET" ? { "X-Proxy-Request": "1" } : {}), ...(data !== undefined ? { "Content-Type": "application/json" } : {}),
    }, ...(data !== undefined ? { body: JSON.stringify(data) } : {}) });
  } catch { throw new Error("Нет соединения с сервисом. Проверьте подключение и повторите запрос."); }
  if (!response.ok) {
    let body; try { body = await response.json(); } catch { body = {}; }
    if (response.status === 401 && path !== "/api/auth/login") showLogin();
    throw new Error(messages[body.error?.code] || (response.status === 413 ? messages.import_too_large : `Не удалось выполнить запрос (HTTP ${response.status}).`));
  }
  return response.status === 204 ? null : raw ? response.text() : response.json();
}
async function action(button, errorId, callback) {
  button.disabled = true; setError(errorId, null);
  try { await callback(); } catch (error) { setError(errorId, error); }
  finally { button.disabled = false; }
}
async function copy(text) {
  try { await navigator.clipboard.writeText(text); notify("Скопировано"); }
  catch { throw new Error("Браузер запретил копирование. Разрешите доступ к буферу обмена или скачайте TXT."); }
}
function filters() {
  const params = new URLSearchParams();
  for (const [name, id] of [["search", "search"], ["protocol", "protocol-filter"], ["region", "region-filter"], ["country", "country-filter"]]) {
    const value = $(id).value.trim(); if (value) params.set(name, value);
  }
  return params;
}
function option(select, value, label) { select.add(new Option(label, value)); }
async function loadCountries() {
  if (countriesLoaded) return;
  const countries = await api("/api/countries");
  for (const id of ["import-region", "region-filter"]) {
    for (const [code, name] of Object.entries(regions)) option($(id), code, name);
  }
  const sorted = countries.sort((a, b) => countryNames.of(a).localeCompare(countryNames.of(b), "en"));
  for (const id of ["import-country", "country-filter"]) {
    option($(id), "unknown", "Unknown");
    for (const code of sorted) option($(id), code, countryName(code));
  }
  countriesLoaded = true;
}
async function showApp(user) {
  await loadCountries();
  $("account-name").textContent = user.username;
  $("login-view").hidden = true; $("loading").hidden = true; $("app-view").hidden = false;
  $("login-form").elements.password.value = "";
  page = 1; await loadList();
}
function updateSelection() {
  $("delete-button").hidden = selected.size === 0;
  $("delete-button").textContent = `Удалить выбранные (${selected.size})`;
  $("select-all").checked = items.length > 0 && selected.size === items.length;
  $("select-all").indeterminate = selected.size > 0 && selected.size < items.length;
}
async function loadList() {
  const version = ++listVersion;
  const params = filters(); params.set("page", page); params.set("per_page", "50");
  setError("list-error", null); $("list-count").textContent = "Загрузка…";
  try {
    const data = await api(`/api/proxies?${params}`);
    if (version !== listVersion) return;
    if (data.items.length === 0 && data.total > 0 && page > 1) { page = Math.ceil(data.total / 50); return loadList(); }
    items = data.items; total = data.total; selected.clear();
    const rows = items.map((item) => {
      const row = document.createElement("tr");
      const checkCell = cell("", "selection"); const checkbox = document.createElement("input");
      checkbox.type = "checkbox"; checkbox.setAttribute("aria-label", `Выбрать ${endpoint(item)}`);
      checkbox.addEventListener("change", () => { checkbox.checked ? selected.add(item.id) : selected.delete(item.id); updateSelection(); });
      checkCell.append(checkbox);
      const location = cell(regions[item.region] || item.region, "location-cell");
      const country = document.createElement("span"); country.textContent = countryName(item.country); location.append(country);
      const expiry = cell(expiryLabel(item.expires_at), "expiry-cell");
      if (item.expired) { const status = document.createElement("span"); status.className = "expired-status"; status.textContent = "Просрочен"; expiry.append(status); }
      const editExpiry = document.createElement("button"); editExpiry.type = "button"; editExpiry.textContent = "Изменить";
      editExpiry.setAttribute("aria-label", `Изменить срок действия ${endpoint(item)}`);
      editExpiry.addEventListener("click", () => {
        $("expiry-form").dataset.id = item.id; $("expiry-endpoint").textContent = endpoint(item);
        $("expiry-date").value = localDateTime(item.expires_at); setError("expiry-error", null); $("expiry-dialog").showModal();
      });
      expiry.append(editExpiry);
      const actions = cell("", "row-action"); const button = document.createElement("button"); button.type = "button"; button.textContent = "Копировать";
      button.disabled = item.expired;
      if (item.expired) button.title = "Срок действия прокси истёк";
      button.setAttribute("aria-label", `Копировать ${endpoint(item)}`);
      button.addEventListener("click", () => action(button, "list-error", async () => { const proxy = await api(`/api/proxies/${item.id}`); await copy(proxy.url); }));
      actions.append(button);
      row.append(checkCell, cell(endpoint(item), "endpoint"), cell(item.protocol.toUpperCase()), location, cell(item.username || "—", "login-cell"), cell(item.username ? "••••••••" : "—"), expiry, actions);
      return row;
    });
    $("proxy-rows").replaceChildren(...rows);
    $("list-count").textContent = `Всего: ${total.toLocaleString("ru-RU")}`;
    $("empty-state").hidden = items.length !== 0;
    const filtered = filters().size > 0;
    $("empty-title").textContent = filtered ? "Ничего не найдено" : "Прокси пока нет";
    $("empty-description").textContent = filtered ? "Попробуйте другой поиск, регион или страну." : "Вставьте список или загрузите TXT / CSV.";
    $("empty-import-button").hidden = filtered;
    $("page-info").textContent = total ? `${(page - 1) * 50 + 1}–${Math.min(page * 50, total)} из ${total.toLocaleString("ru-RU")}` : "0 записей";
    $("prev-page").disabled = page <= 1; $("next-page").disabled = page * 50 >= total;
    $("export-button").disabled = total === 0;
    updateSelection();
  } catch (error) { if (version === listVersion) { setError("list-error", error); $("list-count").textContent = "Список не загружен"; } }
}

$("login-form").addEventListener("submit", (event) => {
  event.preventDefault(); const form = event.currentTarget;
  action(form.querySelector("button"), "login-error", async () => {
    const user = await api("/api/auth/login", { method: "POST", data: { username: form.elements.username.value, password: form.elements.password.value } });
    await showApp(user);
  });
});
$("logout-button").addEventListener("click", () => action($("logout-button"), "list-error", async () => { await api("/api/auth/logout", { method: "POST" }); showLogin(); }));
let searchTimer;
$("search").addEventListener("input", () => { clearTimeout(searchTimer); searchTimer = setTimeout(() => { page = 1; loadList(); }, 250); });
for (const id of ["protocol-filter", "region-filter", "country-filter"]) $(id).addEventListener("change", () => { page = 1; loadList(); });
$("prev-page").addEventListener("click", () => { if (page > 1) { page--; loadList(); } });
$("next-page").addEventListener("click", () => { if (page * 50 < total) { page++; loadList(); } });
$("select-all").addEventListener("change", (event) => {
  selected = new Set(event.target.checked ? items.map((item) => item.id) : []);
  $("proxy-rows").querySelectorAll('input[type="checkbox"]').forEach((checkbox) => { checkbox.checked = event.target.checked; }); updateSelection();
});

function invalidatePreview() {
  previewVersion++; previewData = null; $("import-preview").hidden = true; $("confirm-import").disabled = true; $("skip-invalid").checked = false;
}
function importInput() {
  return { text: $("import-text").value, format: $("import-format").value, protocol: $("import-protocol").value,
    region: $("import-region").value, country: $("import-country").value, expires_at: expiryValue("import-expires-at"), skip_invalid: $("skip-invalid").checked };
}
function updateImportButton() { $("confirm-import").disabled = !previewData || previewData.new === 0 || (previewData.invalid > 0 && !$("skip-invalid").checked); }
function openImport() { setError("import-error", null); $("import-dialog").showModal(); }
for (const id of ["import-button", "empty-import-button"]) $(id).addEventListener("click", openImport);
for (const id of ["import-text", "import-format", "import-protocol", "import-region", "import-country", "import-expires-at"]) $(id).addEventListener("input", invalidatePreview);
$("skip-invalid").addEventListener("change", updateImportButton);
$("import-file").addEventListener("change", async (event) => {
  invalidatePreview(); setError("import-error", null);
  const file = event.target.files[0]; if (!file) return;
  if (file.size > 2 * 1024 * 1024) { setError("import-error", new Error(messages.import_too_large)); event.target.value = ""; return; }
  try {
    const content = new TextDecoder("utf-8", { fatal: true }).decode(await file.arrayBuffer());
    $("import-text").value = content; $("file-name").textContent = file.name;
  } catch { setError("import-error", new Error("Не удалось прочитать файл. Сохраните его в UTF-8.")); }
});
$("import-form").addEventListener("submit", (event) => {
  event.preventDefault(); const version = ++previewVersion;
  action($("preview-button"), "import-error", async () => {
    const data = await api("/api/proxies/preview", { method: "POST", data: importInput() });
    if (version !== previewVersion) return;
    previewData = data; $("import-preview").hidden = false;
    $("preview-summary").textContent = `Новых: ${data.new} · Дублей: ${data.duplicates} · Ошибок: ${data.invalid} · ${regions[data.region]} / ${countryName(data.country)}`;
    $("preview-errors").replaceChildren(...data.errors.map((error) => { const p = document.createElement("p"); p.textContent = `Строка ${error.line}: ${messages[error.code] || error.message}`; return p; }));
    $("skip-invalid-label").hidden = data.invalid === 0;
    $("preview-rows").replaceChildren(...data.preview.map((proxy) => { const row = document.createElement("tr"); row.append(cell(endpoint(proxy)), cell(proxy.protocol.toUpperCase()), cell(proxy.username || "—")); return row; }));
    updateImportButton();
  });
});
$("confirm-import").addEventListener("click", async () => {
  if (!previewData || $("confirm-import").disabled) return;
  await action($("confirm-import"), "import-error", async () => {
    const data = await api("/api/proxies/import", { method: "POST", data: importInput() });
    $("import-dialog").close(); page = 1; await loadList();
    notify(`Добавлено: ${data.inserted}. Дублей: ${data.duplicates}. Ошибок пропущено: ${data.invalid}.`);
  });
  updateImportButton();
});
$("import-dialog").addEventListener("close", () => {
  $("import-form").reset(); $("file-name").textContent = "До 10 000 строк · 2 МБ";
  $("preview-rows").replaceChildren(); $("preview-errors").replaceChildren(); invalidatePreview();
});

$("clear-expiry").addEventListener("click", () => { $("expiry-date").value = ""; $("expiry-date").focus(); });
$("expiry-form").addEventListener("submit", (event) => {
  event.preventDefault();
  action($("save-expiry"), "expiry-error", async () => {
    await api(`/api/proxies/${$("expiry-form").dataset.id}`, { method: "PATCH", data: { expires_at: expiryValue("expiry-date") } });
    $("expiry-dialog").close(); await loadList(); notify("Срок действия обновлён");
  });
});

$("export-button").addEventListener("click", () => { setError("export-error", null); $("export-dialog").showModal(); });
async function exportText() { const params = filters(); params.set("format", $("export-format").value); return api(`/api/proxies/export?${params}`, { raw: true }); }
$("copy-list").addEventListener("click", () => action($("copy-list"), "export-error", async () => copy(await exportText())));
$("download-list").addEventListener("click", () => action($("download-list"), "export-error", async () => {
  const text = await exportText(); const url = URL.createObjectURL(new Blob([text], { type: "text/plain;charset=utf-8" }));
  const link = document.createElement("a"); link.href = url; link.download = "proxies.txt"; document.body.append(link); link.click(); link.remove();
  setTimeout(() => URL.revokeObjectURL(url), 1000); notify("Файл подготовлен");
}));

$("delete-button").addEventListener("click", () => {
  $("delete-description").textContent = `Будет удалено записей: ${selected.size}. Их можно будет добавить заново через импорт.`;
  setError("delete-error", null); $("delete-dialog").showModal();
});
$("confirm-delete").addEventListener("click", () => action($("confirm-delete"), "delete-error", async () => {
  const data = await api("/api/proxies/delete", { method: "POST", data: { ids: [...selected] } });
  $("delete-dialog").close(); await loadList(); notify(`Удалено: ${data.deleted}`);
}));

async function loadKeys() {
  const keys = await api("/api/keys");
  if (!keys.length) { $("key-list").textContent = "API-ключей пока нет."; return; }
  $("key-list").replaceChildren(...keys.map((key) => {
    const row = document.createElement("div"); row.className = "key-row";
    const name = document.createElement("span"); name.textContent = key.name;
    const suffix = document.createElement("code"); suffix.textContent = `…${key.suffix}`; name.append(suffix);
    const button = document.createElement("button"); button.type = "button"; button.className = "danger quiet"; button.textContent = "Отозвать";
    button.addEventListener("click", () => action(button, "api-error", async () => {
      await api(`/api/keys/${key.id}`, { method: "DELETE" }); await loadKeys();
      if ($("new-key").dataset.id === key.id) { $("new-key").hidden = true; $("new-key-value").value = ""; }
      notify("Ключ отозван");
    }));
    row.append(name, button); return row;
  }));
}
$("api-button").addEventListener("click", () => action($("api-button"), "api-error", async () => {
  $("api-dialog").showModal();
  const base = location.origin;
  $("api-example").textContent = [
    `# Любой прокси\ncurl '${base}/api/proxies/random' \\\n  -H 'Authorization: Bearer YOUR_API_KEY'`,
    `# Регион\ncurl '${base}/api/proxies/random?region=europe' \\\n  -H 'Authorization: Bearer YOUR_API_KEY'`,
    `# Страна\ncurl '${base}/api/proxies/random?country=DE' \\\n  -H 'Authorization: Bearer YOUR_API_KEY'`,
    `# Все фильтры вместе\ncurl '${base}/api/proxies/random?region=europe&country=DE&protocol=http' \\\n  -H 'Authorization: Bearer YOUR_API_KEY'`,
  ].join("\n\n");
  await loadKeys();
}));
$("key-form").addEventListener("submit", (event) => {
  event.preventDefault(); action(event.currentTarget.querySelector("button"), "api-error", async () => {
    const key = await api("/api/keys", { method: "POST", data: { name: $("key-name").value } });
    $("new-key-value").value = key.token; $("new-key").dataset.id = key.id; $("new-key").hidden = false;
    $("key-name").value = ""; await loadKeys();
  });
});
$("copy-key").addEventListener("click", () => action($("copy-key"), "api-error", async () => copy($("new-key-value").value)));
$("api-dialog").addEventListener("close", () => { $("new-key-value").value = ""; $("new-key").hidden = true; $("key-name").value = ""; });
document.querySelectorAll("[data-close]").forEach((button) => button.addEventListener("click", () => $(button.dataset.close).close()));

(async () => {
  try { await showApp(await api("/api/auth/me")); }
  catch (error) { showLogin(); if (error.message !== messages.unauthorized) setError("login-error", error); }
})();
