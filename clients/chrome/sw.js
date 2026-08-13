// Service worker — context-menu clipper + side-panel opener. All network I/O
// lives in the side panel / options pages; the worker only shuttles the
// user's page selection into session storage for the panel to pick up.
chrome.runtime.onInstalled.addListener(() => {
  chrome.contextMenus.create({
    id: 'duduclaw-clip',
    title: '把選取內容傳給 DuDuClaw',
    contexts: ['selection'],
  });
  chrome.sidePanel
    .setPanelBehavior({ openPanelOnActionClick: true })
    .catch(() => {});
});

chrome.contextMenus.onClicked.addListener(async (info, tab) => {
  if (info.menuItemId !== 'duduclaw-clip' || !tab?.id) return;
  const clip = {
    text: info.selectionText || '',
    url: info.pageUrl || tab.url || '',
    title: tab.title || '',
    at: Date.now(),
  };
  await chrome.storage.session.set({ pendingClip: clip });
  try {
    await chrome.sidePanel.open({ tabId: tab.id });
  } catch {
    // Side panel may fail on restricted pages; the clip stays queued and the
    // panel picks it up next time it opens.
  }
});
