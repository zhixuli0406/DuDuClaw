# DuDuClaw Project Guidelines

## Design Context

### Users
DuDuClaw is a **personal AI assistant platform** for individual developers and power users, primarily in Taiwan (zh-TW). Users interact through a web dashboard to manage multiple AI agents, monitor channels (LINE/Telegram/Discord), track API budgets, and observe their agents' self-evolution. They expect a tool that feels like a trusted companion — not a cold enterprise console.

### Brand Personality
**Professional · Efficient · Precise** — with a warm, approachable surface.

Like a skilled engineer who happens to be your close friend: reliable, sharp, but never cold. The paw print (🐾) icon reflects a pet-like companionship — the AI is loyal, attentive, and delightful to interact with.

### Aesthetic Direction
- **Primary references**: Claude.ai (warm sand/beige tones, generous whitespace, soft typography) + Raycast (macOS-native polish, frosted glass effects, refined dark theme)
- **Anti-references**: Grafana (too dense), Discord (too playful), enterprise dashboards (too cold)
- **Color palette**:
  - Primary: warm amber (`amber-500` / `#f59e0b`) — evokes warmth and trust
  - Accent: soft orange (`orange-400` / `#fb923c`) — for highlights and CTAs
  - Surface light: warm stone (`stone-50` / `#fafaf9`) with subtle warm undertones
  - Surface dark: deep stone (`stone-900` / `#1c1917`) — warm dark, not cold blue-black
  - Success: emerald, Warning: amber, Error: rose — standard semantic colors
- **Theme**: Follow system preference (auto dark/light), with manual toggle
- **Typography**: System font stack for performance; generous line-height; larger body text (16px base)
- **Border radius**: Rounded (0.75rem default) — soft, approachable
- **Spacing**: Generous padding — the interface should breathe
- **Motion**: Subtle fade-in/slide transitions (150-200ms); respect `prefers-reduced-motion`
- **Glass effects**: Subtle backdrop-blur on sidebars and overlays (Raycast influence)

### Design Principles
1. **Warmth over sterility** — Every surface should feel inviting. Prefer warm neutrals over cold grays. Use color strategically to create emotional connection.
2. **Clarity over density** — Show what matters, hide what doesn't. Progressive disclosure: summary first, details on demand. Never overwhelm.
3. **Real-time without anxiety** — Status indicators should inform, not alarm. Use gentle transitions for state changes. Green means "all is well" and should be the dominant state color.
4. **One binary, one experience** — The dashboard is embedded in the Rust binary. It should feel native and instant, like a local app, not a remote web service.
5. **Accessible by default** — WCAG 2.1 AA compliance. Semantic HTML. Keyboard navigation. Respect motion preferences. Sufficient color contrast in both themes.
