# Physical touchpoint guide: QR table cards, NFC tags, and LINE Touch

> Audience: shops and implementation partners putting an AI employee on LINE. Goal: let a customer in the store scan or tap once and start chatting with your AI employee right away.
> Prerequisite: the Channels page already has a LINE Official Account connected (Channels page → "LINE Add Friend QR" (LINE 加好友 QR) showing your QR code means this step is done).

## 1. QR table card or poster (lowest cost, 5 minutes)

1. Dashboard → Channels → **"LINE Add Friend QR"** (LINE 加好友 QR).
2. Click "Print poster" (列印海報) to print directly (browser printing can also save as a PDF), or "Copy link" (複製連結) to paste into your own design.
3. Place it on the counter, tables, packaging, or business cards. A customer scans the code, adds the account as a friend, and starts chatting immediately; one-to-one replies go through the LINE Reply API and **cost nothing**.

## 2. DIY NFC table card (tap to start chatting)

Shops in Taiwan already use "Google review NFC stickers" widely. The same action works for your AI employee too:

| Material | Notes | Reference cost |
|---|---|---|
| NFC tag | NTAG213 (144 bytes, more than enough for one URL) | About NT$22-35 each at retail; bulk custom production with printing costs less |
| Writer app | NFC Tools (iOS/Android, free) or any NDEF writer | Free |
| Table card | Acrylic stand plus sticker, or an anti-metal NFC sticker on its own | Tens to a couple hundred NT dollars |

Steps:
1. Channels page → "LINE Add Friend QR" (LINE 加好友 QR) → **Copy link** (this is the URL the NFC tag will hold).
2. NFC Tools → Write → Add a record → URL → paste → Write. Hold the phone near the tag to finish writing.
3. **Lock the tag** afterward so it cannot be overwritten.
4. Test it: bring a second phone close; it should jump straight to the add-friend page.

Print both a QR code and an NFC tag on the same card: older iPhone models and some Android phones vary in NFC support, so the QR code is the fallback.

## 3. LINE Touch (LINE's official NFC program)

LINE's own "tap" program. Compared with DIY NFC: tags are issued by LINE, the destination can be changed anytime from the OA admin console (no need to swap the physical tag), it can point to a LINE MINI App, and it comes with official marketing support.

**Readiness checklist (as of 2026-08)**:

- [ ] **Blue-badge verified Official Account**: a prerequisite for LINE Touch. Applying yourself takes up to 30 days to hear back, typically around 10 business days; expedited review through an agency takes about 5 days.
- [ ] **Tag purchase**: LINE Touch NFC tags are expected to go on sale in the OA Shop starting **late September 2026** (two sizes: A6 and 54x85mm); tags can only be bought from LINE, not produced on your own.
- [ ] **Console setup**: the OA admin console (the LINE Touch feature area opened on 2026-07-22) binds the destination: add friend, a specific message, or a MINI App.
- [ ] **Destination recommendation**: start by pointing to "add friend" (the official version of the same link used in section 1 of this guide); upgrade the destination once a MINI App exists.

Choosing between DIY NFC and LINE Touch: **need it today, go DIY** (section 2 of this guide, live the same day); **need an editable official console, or need a MINI App, go LINE Touch** (wait for tags to go on sale in September). The two are not exclusive: start with DIY, then switch once the tags arrive.

## 4. Notes for implementation partners (resellers)

- Co-branded cards work: partner with a Taiwan NFC print shop (custom stickers or acrylic stands) for bulk production. COGS per table card runs about NT$100-200, which works as a bundled gift or an add-on purchase in an onboarding package.
- Before shipping, test every batch of tags yourself on your own phone: write, lock, and redirect, all three steps.

---
Last updated: 2026-08-13. The LINE Touch timeline restates information LINE has made public; purchase details follow whatever appears in the OA Shop at launch.
