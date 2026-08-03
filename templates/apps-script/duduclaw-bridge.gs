/**
 * DuDuClaw Apps Script bridge — Google Workspace access without an OAuth client.
 *
 * WHY THIS EXISTS
 * The normal path (DuDuClaw's native Google tools) needs an OAuth client that
 * Google has verified before it may serve users outside the developer's own
 * domain. This bridge sidesteps that: the script runs inside YOUR account,
 * under YOUR authorization, so there is no third-party app to verify and no
 * client id to create. It also works for personal @gmail.com accounts, which
 * domain-wide delegation cannot reach.
 *
 * WHAT YOU ARE AGREEING TO
 * The deployed URL plus the shared secret together act as a credential for your
 * mailbox, calendar and spreadsheets. Treat the pair exactly like a password:
 * never paste them into a chat, an issue, or a screenshot. Rotate by changing
 * SECRET below and redeploying — the old secret stops working immediately.
 *
 * SETUP (about five minutes, once)
 *   1. Open https://script.google.com and create a new project.
 *   2. Replace the whole file with this one.
 *   3. Change SECRET below to a long random string (30+ chars). Generate one
 *      with: openssl rand -base64 32
 *   4. Deploy → New deployment → type "Web app".
 *        Execute as:      Me
 *        Who has access:  Anyone
 *      ("Anyone" is what lets DuDuClaw reach the URL. The secret is what keeps
 *      strangers out — anyone who guesses the URL still gets 'unauthorized'.)
 *   5. Google shows a consent screen for YOUR OWN script. It warns that the app
 *      is unverified; that is expected, because this script is yours, not a
 *      third party's. Choose Advanced → Go to (project name).
 *   6. Copy the "/exec" web app URL.
 *   7. In DuDuClaw: 管理 → 整合／工具連線 → Google，貼上網址與密鑰。
 *
 * QUOTAS
 * Apps Script enforces daily limits per account (consumer accounts are the
 * tightest: ~100 Gmail reads/day on the free tier at time of writing, higher on
 * Workspace). This bridge is for interactive assistant use, not bulk sync.
 */

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

/** CHANGE THIS. Long random string; must match what you paste into DuDuClaw. */
var SECRET = 'CHANGE_ME_TO_A_LONG_RANDOM_STRING';

/** Hard caps so one call can never return an unbounded payload. */
var MAX_RESULTS = 25;
var MAX_BODY_CHARS = 4000;
var MAX_SHEET_ROWS = 200;

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

function doPost(e) {
  try {
    var req = JSON.parse(e.postData.contents);
    if (!secretMatches(req.secret)) {
      // Deliberately vague: never reveal whether the URL or the secret was the
      // wrong half.
      return json({ error: 'unauthorized' });
    }
    var params = req.params || {};
    switch (req.action) {
      case 'status':                return json(actionStatus());
      case 'gmail_search':          return json(actionGmailSearch(params));
      case 'gmail_read':            return json(actionGmailRead(params));
      case 'gmail_create_draft':    return json(actionGmailCreateDraft(params));
      case 'calendar_list_events':  return json(actionCalendarList(params));
      case 'calendar_create_event': return json(actionCalendarCreate(params));
      case 'sheets_read':           return json(actionSheetsRead(params));
      case 'sheets_append':         return json(actionSheetsAppend(params));
      default:
        return json({ error: 'unknown action: ' + String(req.action) });
    }
  } catch (err) {
    // Message only — a stack trace can carry document names and addresses.
    return json({ error: String(err && err.message ? err.message : err) });
  }
}

/**
 * Length-then-full-scan comparison. Apps Script has no timing-safe primitive,
 * and network jitter dominates any residual signal, but scanning every
 * character instead of returning at the first mismatch costs nothing.
 */
function secretMatches(given) {
  if (typeof given !== 'string') return false;
  if (given.length !== SECRET.length) return false;
  var diff = 0;
  for (var i = 0; i < SECRET.length; i++) {
    diff |= given.charCodeAt(i) ^ SECRET.charCodeAt(i);
  }
  return diff === 0;
}

function json(obj) {
  return ContentService
    .createTextOutput(JSON.stringify(obj))
    .setMimeType(ContentService.MimeType.JSON);
}

function clampChars(s, max) {
  s = String(s == null ? '' : s);
  return s.length > max ? s.substring(0, max) : s;
}

function clampCount(n, fallback) {
  n = parseInt(n, 10);
  if (!(n > 0)) return fallback;
  return Math.min(n, MAX_RESULTS);
}

// ---------------------------------------------------------------------------
// Actions
// ---------------------------------------------------------------------------

/** Connection check — proves the URL, the secret and the grants all work. */
function actionStatus() {
  return {
    ok: true,
    account: Session.getActiveUser().getEmail(),
    timezone: Session.getScriptTimeZone(),
    bridge_version: 1
  };
}

/** Search mail with Gmail query syntax ("from:x is:unread newer_than:3d"). */
function actionGmailSearch(p) {
  var limit = clampCount(p.limit, 10);
  var threads = GmailApp.search(String(p.query || ''), 0, limit);
  var out = [];
  for (var i = 0; i < threads.length && out.length < limit; i++) {
    var m = threads[i].getMessages()[0];
    out.push({
      message_id: m.getId(),
      subject: clampChars(m.getSubject(), 200),
      from: clampChars(m.getFrom(), 200),
      date: m.getDate().toISOString(),
      unread: m.isUnread(),
      snippet: clampChars(m.getPlainBody(), 200)
    });
  }
  return { messages: out };
}

/** Read one message in full (plain text, truncated). */
function actionGmailRead(p) {
  var m = GmailApp.getMessageById(String(p.message_id || ''));
  if (!m) return { error: 'message not found' };
  var body = m.getPlainBody();
  // Attachment NAMES only — this bridge never returns file bytes.
  var atts = m.getAttachments().map(function (a) {
    return { name: clampChars(a.getName(), 200), size: a.getSize() };
  });
  return {
    message_id: m.getId(),
    subject: clampChars(m.getSubject(), 200),
    from: clampChars(m.getFrom(), 200),
    to: clampChars(m.getTo(), 200),
    date: m.getDate().toISOString(),
    body: clampChars(body, MAX_BODY_CHARS),
    truncated: body.length > MAX_BODY_CHARS,
    attachments: atts
  };
}

/**
 * Create a DRAFT. There is deliberately no send action in this bridge: the
 * native tools hold the same line, so "the AI can prepare mail but a human
 * presses send" survives whichever path a customer is on.
 */
function actionGmailCreateDraft(p) {
  var draft = GmailApp.createDraft(
    String(p.to || ''),
    String(p.subject || ''),
    String(p.body || '')
  );
  return { draft_id: draft.getId(), message_id: draft.getMessage().getId() };
}

/** Upcoming events, default the next 7 days. */
function actionCalendarList(p) {
  var days = parseInt(p.days, 10);
  if (!(days > 0)) days = 7;
  var now = new Date();
  var until = new Date(now.getTime() + days * 86400000);
  var events = CalendarApp.getDefaultCalendar().getEvents(now, until);
  var out = [];
  for (var i = 0; i < events.length && out.length < MAX_RESULTS; i++) {
    var ev = events[i];
    out.push({
      id: ev.getId(),
      title: clampChars(ev.getTitle(), 200),
      start: ev.getStartTime().toISOString(),
      end: ev.getEndTime().toISOString(),
      location: clampChars(ev.getLocation(), 200)
    });
  }
  return { events: out };
}

/** Create a real, externally visible event. */
function actionCalendarCreate(p) {
  var start = new Date(String(p.start || ''));
  var end = new Date(String(p.end || ''));
  if (isNaN(start.getTime()) || isNaN(end.getTime())) {
    return { error: 'start and end must be ISO-8601 timestamps' };
  }
  var ev = CalendarApp.getDefaultCalendar().createEvent(
    String(p.title || '(no title)'),
    start,
    end,
    { description: String(p.description || ''), location: String(p.location || '') }
  );
  return { id: ev.getId(), title: ev.getTitle(), start: ev.getStartTime().toISOString() };
}

/** Read a cell range. `spreadsheet` accepts an id or a full sheet URL. */
function actionSheetsRead(p) {
  var ss = openSpreadsheet(String(p.spreadsheet || ''));
  var range = String(p.range || '');
  var values = range ? ss.getRange(range).getValues() : ss.getSheets()[0].getDataRange().getValues();
  if (values.length > MAX_SHEET_ROWS) values = values.slice(0, MAX_SHEET_ROWS);
  return { rows: values.length, values: values };
}

/** Append one row. */
function actionSheetsAppend(p) {
  var ss = openSpreadsheet(String(p.spreadsheet || ''));
  var values = p.values;
  if (!Array.isArray(values)) return { error: 'values must be an array' };
  var sheet = p.sheet ? ss.getSheetByName(String(p.sheet)) : ss.getSheets()[0];
  if (!sheet) return { error: 'sheet not found' };
  sheet.appendRow(values);
  return { appended: true, row: sheet.getLastRow() };
}

/** Accept either a bare spreadsheet id or a full /spreadsheets/d/<id>/ URL. */
function openSpreadsheet(idOrUrl) {
  var m = idOrUrl.match(/\/spreadsheets\/d\/([a-zA-Z0-9-_]+)/);
  return SpreadsheetApp.openById(m ? m[1] : idOrUrl);
}
