# Add teammates and guide their first login

Administrators can create up to ten teammates at once from **Users → Add teammates**. Enter each person's name, email, role and optional department. Choose an AI avatar and any additional employees, with **Can operate** or **View only** access for each. The AI avatar is the default for a new conversation or assignment when the person has the required access; choosing a default does not grant extra permissions. The machine employee requires its existing separate authorization and cannot be bound through this form.

Each AI can be only one member’s avatar. Choose another AI if it is already assigned. An avatar is fixed by an administrator; members cannot switch it themselves. Offboarding frees the avatar without changing the existing transfer process.

Leave **Send invitation** checked to send a one-time password setup link. **Preview invitation** shows the saved content in the current dashboard language; its example link cannot activate an account. Creating an account never assigns a shared or temporary password. Uncheck delivery to create the account and bindings first, then use **Invite** in the user list when ready.

Each row is a separate database transaction: account, bindings, avatar selection, invitation credential (when requested), first-login progress and audit entry either all succeed or all roll back. A failed row does not undo successful rows. Delivery happens after that transaction: if email fails, the account remains created and the administrator can copy its link or issue a new invitation. Resending invalidates the older link. Successful rows are not resubmitted when retrying failed rows. If the browser loses the response, check the user list before trying again.

Invitations use the existing mail settings and recipient policy. Set the public dashboard URL in gateway configuration; otherwise the form supplies the dashboard's current origin. Every invitation public URL must use HTTPS, including localhost and other loopback addresses. Links expire after seven days and can be used once. Only token hashes are stored. The person sets a password and then signs in normally, including any required second factor.

## Edit the invitation

Open **Settings → Invitation email** as an administrator. Choose Traditional Chinese, English or Japanese and edit the opening, setup steps and three examples independently for each language. The defaults cover summarizing a document, drafting a reply and setting a reminder. An optional course link must use HTTPS and contain no embedded username or password.

The live preview and delivered email use the same renderer. Text is escaped, paired `**bold**` is supported, and line breaks are kept. Raw HTML, images and Markdown links are not interpreted. The password link and expiry notice are supplied by the system. Previewing does not create a live invitation or send email.

## First login

After an invited person sets their password and signs in, their home page shows the **A few steps left** onboarding card (same title as the personal connections card; it contains the five items below):

- Password setup is checked from the account's state.
- Channel binding is done by an administrator after checking the person's channel identity; it can be skipped.
- Google connection opens **My connections**; it can be skipped. The card records saved connection status, not a live Google API test.
- An example opens a new conversation with a draft. It does not send the message automatically.
- **What AI employees can and cannot do** explains capabilities, required approvals, mistakes and privacy; mark it read after reviewing it.

Progress is saved for the signed-in person on the server. Connection steps are completed from saved verified connection records, not from a client claiming success. The card disappears when every step is completed or skipped, or when the person dismisses it. This checklist grants no permissions and does not bypass password or MFA gates. Existing accounts are not enrolled by the migration; redeeming an existing invitation also enables the card.

## Verification boundary

The change includes database and RPC tests. Delivery through real SMTP, first login in a running gateway, and browser interaction still need release acceptance; unit tests alone do not establish those outcomes.

The AI avatar must have **Can operate** access (existing Owner bindings also qualify). **View only** is available for additional employees and cannot be selected as an avatar or used to start or resume Dashboard chats. Legacy avatar selections are rechecked against live access; downgrading a binding removes it from chat candidates. Invitation and course URLs require HTTPS without embedded credentials, including localhost.

## AI avatars and licensed seats

**AI avatars do not use employee seats** when the AI is the Personal AI of exactly one active member and that member has Owner access. A shared employee assigned as an avatar, an Operator-only binding, a profile flag, or an unreadable identity record does not qualify. Archived AI still use seats; deleted AI do not. The Users, AI employees and License pages show ordinary seats and exempt avatars separately.

Changing or clearing an avatar, removing its Owner binding, suspending or offboarding its member, or archiving the AI is refused if the resulting seat count would exceed the limit. Delete an unused AI or increase the licensed limit first. The Admin RPC `users.set_avatar` also accepts `delete_avatar: true` together with `agent_id: null` to delete the assigned AI and clear the assignment in the same request; deletion includes the normal credential cleanup and Personal write authorization (including `step_up_csrf` when required in native isolation). If cleanup fails, the RPC reports that deletion has happened and the remaining cleanup must be retried.

An Admin can create a new Personal avatar even when ordinary seats are full with `agents.create` parameters `{ "name": "member-avatar", "avatar_for_user": "MEMBER_ID" }`. The member must be active and have no avatar. This creates the Personal ownership record, Owner binding and avatar assignment together; it cannot convert an existing ordinary employee. For native Linux isolation, use the Admin `isolation.employee_propose` RPC with `binding_kind: "personal"`, `owner_id` and `as_avatar: true`. It creates the same Personal avatar while preserving step-up and the existing root allocation approval; the AI remains inert until approved. Existing RM-212 assignments qualify only after their live Owner and Personal records have been verified; migration does not turn shared AI into Personal AI. `license.status` reports `seats_used` and `avatars_exempt`; unavailable counts are `null`.
