# Add teammates and guide their first login

Administrators can create up to ten teammates at once from **Users → Add teammates**. Enter each person's name, email, role and optional department. Choose a primary AI employee and any additional employees, with **Can operate** or **View only** access for each. The primary employee is the default for a new conversation or assignment when the person has the required access; choosing a default does not grant extra permissions. The machine employee requires its existing separate authorization and cannot be bound through this form.

Leave **Send invitation** checked to send a one-time password setup link. **Preview invitation** shows the saved content in the current dashboard language; its example link cannot activate an account. Creating an account never assigns a shared or temporary password. Uncheck delivery to create the account and bindings first, then use **Invite** in the user list when ready.

Each row is a separate database transaction: account, bindings, primary selection, invitation credential (when requested), first-login progress and audit entry either all succeed or all roll back. A failed row does not undo successful rows. Delivery happens after that transaction: if email fails, the account remains created and the administrator can copy its link or issue a new invitation. Resending invalidates the older link. Successful rows are not resubmitted when retrying failed rows. If the browser loses the response, check the user list before trying again.

Invitations use the existing mail settings and recipient policy. Set the public dashboard URL in gateway configuration; otherwise the form supplies the dashboard's current origin. Every invitation public URL must use HTTPS, including localhost and other loopback addresses. Links expire after seven days and can be used once. Only token hashes are stored. The person sets a password and then signs in normally, including any required second factor.

## Edit the invitation

Open **Settings → Invitation email** as an administrator. Choose Traditional Chinese, English or Japanese and edit the opening, setup steps and three examples independently for each language. The defaults cover summarizing a document, drafting a reply and setting a reminder. An optional course link must use HTTPS and contain no embedded username or password.

The live preview and delivered email use the same renderer. Text is escaped, paired `**bold**` is supported, and line breaks are kept. Raw HTML, images and Markdown links are not interpreted. The password link and expiry notice are supplied by the system. Previewing does not create a live invitation or send email.

## First login

After an invited person sets their password and signs in, their home page shows **A few steps left**:

- Password setup is checked from the account's state.
- Channel binding is done by an administrator after checking the person's channel identity; it can be skipped.
- Google connection opens **My connections**; it can be skipped. The card records saved connection status, not a live Google API test.
- An example opens a new conversation with a draft. It does not send the message automatically.
- **What AI employees can and cannot do** explains capabilities, required approvals, mistakes and privacy; mark it read after reviewing it.

Progress is saved for the signed-in person on the server. Connection steps are completed from saved verified connection records, not from a client claiming success. The card disappears when every step is completed or skipped, or when the person dismisses it. This checklist grants no permissions and does not bypass password or MFA gates. Existing accounts are not enrolled by the migration; redeeming an existing invitation also enables the card.

## Verification boundary

The change includes database and RPC tests. Delivery through real SMTP, first login in a running gateway, and browser interaction still need release acceptance; unit tests alone do not establish those outcomes.

The primary employee must have **Can operate** access (existing Owner bindings also qualify). **View only** is available for additional employees and cannot be selected as primary or used to start or resume Dashboard chats. Legacy primary selections are rechecked against live access; downgrading a binding removes it from chat candidates. Invitation and course URLs require HTTPS without embedded credentials, including localhost.
