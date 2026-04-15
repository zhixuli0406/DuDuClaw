# 5-Layer Browser Automation Router

> Progressive resource escalation — use the lightest tool that gets the job done.

---

## The Metaphor: Getting Information About a Restaurant

You want to know if a restaurant is open tonight. Here's how a reasonable person would approach it:

1. **Check Google Maps** — fastest, cheapest. If the hours are listed, you're done.
2. **Visit the restaurant's website** — a bit more effort, but still quick. Check the homepage for hours.
3. **Call the restaurant** — requires more effort (and their time), but handles cases where the website is outdated.
4. **Send someone to check in person** — expensive and slow, but guaranteed to get the answer.
5. **Send someone to sit down and order** — the most expensive option, but necessary if you need to evaluate the full experience.

DuDuClaw's browser automation follows the same principle: start with the cheapest approach, and only escalate when the previous level fails.

---

## How It Works

### The Five Layers

**Layer 1: API Fetch** — Direct HTTP request to a REST API endpoint. The fastest and cheapest option.

```
Target has a public API?
     |
  +--+--+
  |     |
 Yes    No
  |     |
  v     v
Call    Fall to L2
API
  |
  v
Parse JSON response
Done.
```

When it works: Structured data sources with documented APIs. Weather services, stock prices, public databases.

When it fails: The target has no API, or the data you need isn't exposed through it.

**Layer 2: Static Scrape** — Download the HTML page and parse it without executing JavaScript.

```
Download HTML
     |
     v
Parse DOM structure
     |
     v
Extract target data
     |
  +--+--+
  |     |
Found   Not found (page needs JS to render)
  |     |
  v     v
Done.   Fall to L3
```

When it works: Traditional server-rendered websites where content is in the initial HTML.

When it fails: Single-page applications (SPAs) that require JavaScript to render content.

**Layer 3: Headless Browser** — Launch a browser engine (without a visible window) that executes JavaScript, renders the page fully, and then extracts data.

```
Launch headless browser
     |
     v
Navigate to URL
     |
     v
Wait for JavaScript to render
     |
     v
Extract data from rendered DOM
     |
  +--+--+
  |     |
Found   Not found (needs login, CAPTCHA, etc.)
  |     |
  v     v
Done.   Fall to L4
```

When it works: Modern web apps that render content client-side. Most SPAs, dashboards, interactive maps.

When it fails: Pages that require authentication, CAPTCHA solving, or other interactive steps.

**Layer 4: Sandbox Browser** — Same as L3, but running inside an isolated container for security.

```
Launch container with:
  - No network access (except target URL)
  - Read-only filesystem
  - Temporary storage (wiped after use)
  - Resource limits (CPU, memory, time)
     |
     v
Run headless browser inside container
     |
     v
Perform authenticated browsing / form filling
     |
  +--+--+
  |     |
Done    Too complex (needs visual interaction)
  |     |
  v     v
Return  Fall to L5
data
```

When it works: Authenticated pages, sites that try to detect automation, pages requiring complex interaction sequences.

When it fails: Pages that require visual pattern recognition, drag-and-drop, or other human-like visual interactions.

**Layer 5: Computer Use** — A virtual display with simulated mouse and keyboard. The browser runs visually, and the system "looks" at the screen to decide what to click.

```
Launch virtual display
     |
     v
Open browser (visible on virtual screen)
     |
     v
Screenshot → Visual analysis → Click/Type
     |
     v
Repeat until task complete
```

When it works: Anything that a human sitting at a computer could do. This is the most powerful layer.

When it fails: It doesn't. But it's the slowest and most expensive option, so it should be the last resort.

**Alternative: Browserbase** — For teams that don't want to manage virtual displays locally, DuDuClaw supports Browserbase as a cloud browser alternative for L5. The browser runs in Browserbase's infrastructure, and DuDuClaw sends commands via API. Same capability, zero local resource cost (but requires a Browserbase account).

### The Routing Decision

The router doesn't always start at L1. Based on the task and target, it may skip directly to the appropriate layer:

```
Known API endpoint?      --> Start at L1
Known static site?       --> Start at L2
Known SPA?               --> Start at L3
Requires authentication? --> Start at L4
Requires visual tasks?   --> Start at L5
Unknown?                 --> Start at L1, fall through as needed
```

---

## Security: Deny by Default

Browser automation is powerful — and dangerous. An agent with unrestricted browser access could:
- Visit malicious sites
- Download malware
- Exfiltrate data through web forms
- Execute arbitrary JavaScript

To prevent this, DuDuClaw uses a **deny-by-default** capability model:

```
Agent's configuration:
  [capabilities]
  computer_use = false        # L5 disabled by default
  browser_via_bash = false    # L3/L4 via shell disabled
  allowed_tools = [...]       # Whitelist of permitted tools
  denied_tools = [...]        # Blacklist of forbidden tools
```

Each layer beyond L2 requires explicit authorization in the agent's configuration. An agent that hasn't been specifically granted browser capabilities can only use API calls and static scraping.

For L3/L4 access via shell commands, an additional gatekeeper checks:
- Is the specific browser automation command on the allowlist?
- Is the required environment flag set?
- Does the command match the expected format?

---

## Resource Comparison

| Layer | Startup Time | Memory | Network | Security Risk |
|-------|-------------|--------|---------|---------------|
| L1: API Fetch | ~0ms | ~1 MB | Single HTTP request | Minimal |
| L2: Static Scrape | ~0ms | ~5 MB | Single HTTP request | Low |
| L3: Headless Browser | ~2s | ~200 MB | Multiple requests | Medium |
| L4: Sandbox Browser | ~5s | ~300 MB | Controlled | Low (isolated) |
| L5: Computer Use | ~10s | ~500 MB+ | Full browser | Medium (isolated) |

The difference between L1 and L5 is roughly 500x in resource consumption. Using L5 for a task that L1 can handle is like driving a truck to pick up a letter.

---

## Why This Matters

### Cost Proportional to Complexity

Every web task gets the minimum resources needed to complete it. Simple data retrieval (95% of cases) uses near-zero resources. Only genuinely complex tasks (logins, visual interactions) trigger the heavy layers.

### Security Through Minimization

An agent that only needs to check stock prices doesn't get browser access. The principle of least privilege is enforced automatically through the capability system.

### Graceful Degradation

If a higher layer fails (container service unavailable, browser engine crashes), the system can often fall back to a lower layer with reduced functionality rather than failing entirely.

---

## Interaction with Other Systems

- **Container Sandbox**: L4 uses the same container infrastructure that isolates agent task execution.
- **Security Hooks**: The bash gatekeeper integrates with the broader security defense system.
- **Agent Configuration**: Each agent's browser capabilities are part of its overall capability profile.
- **Audit Log**: All browser automation actions are recorded for compliance.

---

## The Takeaway

The 5-layer browser router embodies a simple principle: don't use a sledgehammer to hang a picture frame. By automatically matching the tool to the task's complexity, the system minimizes resource usage, reduces security exposure, and keeps most web interactions fast and cheap.
