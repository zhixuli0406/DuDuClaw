# Prediction-Driven Evolution Engine

> Let 90% of conversations evolve at zero LLM cost.

---

## The Metaphor: A Seasoned Doctor's Intuition

Imagine a doctor with 30 years of experience. When a patient walks in sneezing with a runny nose, the doctor doesn't order a full blood panel and MRI — they prescribe rest and fluids based on pattern recognition. Only when something *unexpected* appears (unusual symptoms, contradictory signs) does the doctor escalate to expensive diagnostic procedures.

DuDuClaw's evolution engine works the same way. Instead of reflecting on every single conversation (expensive), it predicts what *should* happen, then only invests computational resources when reality deviates from prediction.

---

## How It Works

### The Core Loop

Every time a user message arrives, the engine runs through this cycle:

```
User message arrives
       |
       v
+------------------+
|  Predict outcome |  <-- Based on learned patterns
+--------+---------+
         |
         v
+------------------+
|  Observe actual  |  <-- What the agent actually did
|    response      |
+--------+---------+
         |
         v
+------------------+
| Calculate error  |  <-- How far off was the prediction?
+--------+---------+
         |
         v
+------------------+
|  Route by error  |  <-- Choose the cheapest appropriate action
|    severity      |
+------------------+
```

### The Four Error Levels

The prediction error determines what happens next:

| Error Level | What It Means | What Happens | Cost |
|-------------|---------------|--------------|------|
| **Negligible** | Prediction was spot-on | Nothing. Move on. | Zero |
| **Moderate** | Slightly off, but within tolerance | Record the deviation for future learning. No action. | Zero |
| **Significant** | Meaningfully wrong | Trigger a single GVU reflection cycle | One LLM call |
| **Critical** | Completely wrong | Trigger emergency GVU loop (up to 3 rounds) | Multiple LLM calls |

In practice, the vast majority of conversations land in the first two buckets. The agent's behavior is predictable enough that the engine can confirm "everything is fine" without spending a single API token.

### The Dual-Process Design

This architecture mirrors how human cognition works according to Kahneman's framework:

**System 1 (Fast, automatic):** A set of lightweight heuristic rules handles the common case. These rules check things like:
- Did the response match the expected intent category?
- Was the tone consistent with the agent's personality?
- Did the response stay within behavioral boundaries?

**System 2 (Slow, deliberate):** Only activated when System 1 flags an anomaly. This is where the LLM is called to deeply analyze what went wrong and propose corrections.

The beauty is that System 1 handles ~90% of traffic. System 2 is reserved for the ~10% that genuinely needs reflection.

### Self-Calibrating Thresholds

Here's where it gets clever: the boundary between "Moderate" and "Significant" isn't fixed. The **MetaCognition** module reviews its own performance every 100 predictions:

```
Every 100 predictions:
  |
  v
Are we triggering GVU too often?
  --> Yes: Raise the "Significant" threshold (be more tolerant)
  --> No: Check if we're missing real problems
          --> Yes: Lower the threshold (be more sensitive)
          --> No: Keep current settings
```

This means the system adapts to each agent's unique behavioral profile. An agent that handles customer support (predictable patterns) will have higher thresholds than an agent doing creative writing (inherently unpredictable).

---

## Why This Matters

### Cost Control

Without this engine, every conversation would trigger an LLM-based reflection. At scale (thousands of messages per day), that reflection cost can exceed the cost of the conversations themselves. The prediction-driven approach cuts reflection costs by ~90%.

### Latency

Reflection adds latency to the evolution pipeline. By skipping reflection for 90% of conversations, the evolution system stays responsive and doesn't become a bottleneck.

### Signal-to-Noise

Not all conversations carry equal evolutionary signal. A routine "good morning" exchange teaches the agent nothing new. By filtering through prediction error, the engine focuses its learning budget on conversations that actually contain novel information.

---

## Zero-LLM Conversation Outcome Detection

Before the prediction engine can compare expected vs. actual outcomes, it needs to understand *what happened* in the conversation. The **ConversationOutcome** module classifies every conversation along three dimensions — entirely without LLM calls:

```
Conversation ends
     |
     v
+-----------------------+
| TaskType detection    |  <-- What was the user trying to do?
|   (question, request, |      (keyword + pattern matching)
|    complaint, etc.)   |
+-----------------------+
     |
     v
+-----------------------+
| Satisfaction signal   |  <-- Was the user happy?
|   (explicit feedback, |      (sentiment heuristics,
|    conversation flow) |       re-engagement patterns)
+-----------------------+
     |
     v
+-----------------------+
| Completion detection  |  <-- Was the task finished?
|   (confirmation cues, |      (zh-TW + en dual-language)
|    topic closure)     |
+-----------------------+
```

This provides the *observed reality* that the prediction engine compares against its predictions — all at zero cost.

---

## MistakeNotebook: Cross-Loop Error Memory

When a prediction fails (Significant or Critical error), the details are recorded in the **MistakeNotebook** — a persistent log of failure patterns:

```
Prediction error (Significant or Critical)
     |
     v
Record in MistakeNotebook:
  - What was predicted vs. what happened
  - The conversation context
  - Which SOUL.md version was active
  - The error category
     |
     v
Future predictions check the notebook:
  "Have I made this kind of mistake before?"
     |
     v
  If yes: Lower the confidence threshold for
          similar situations (be more cautious)
```

The notebook feeds into both the prediction engine (improving future predictions) and the GVU loop (the L2.5 MistakeRegression verification layer checks new personality versions against known failure patterns).

---

## Deferred GVU: Patient Evolution

Not every significant prediction error needs immediate action. The **Deferred GVU** mechanism accumulates gradient signals before triggering a full evolution cycle:

```
Significant error detected
     |
     v
Is the gradient buffer full enough?
     |
  +--+--+
  |     |
 Yes    No
  |     |
  v     v
Fire    Accumulate gradient
GVU    (defer for later)
now        |
           v
        Max 3 deferrals across 72 hours
        → 9-21 effective iterations
           spread over days
```

This prevents the evolution engine from reacting to every bump in the road. Instead, it waits until enough evidence accumulates to justify a meaningful change — resulting in more stable, higher-quality evolution.

---

## Interaction with Other Systems

- **GVU Loop**: The prediction engine is the *gatekeeper* for GVU. It decides when GVU fires and with what urgency.
- **MistakeNotebook**: Cross-loop memory that prevents the same mistakes from recurring.
- **ConversationOutcome**: Zero-cost conversation classification that provides the "observed reality" for prediction comparison.
- **SOUL.md Versioning**: When GVU produces a new SOUL.md version, the prediction engine's accuracy is part of the 24-hour observation metrics.
- **CostTelemetry**: The engine's hit/miss ratio is tracked and visible in the dashboard, helping operators understand how much the engine is saving.
- **Deferred GVU**: Gradient accumulation ensures evolution is patient and evidence-based.

---

## The Takeaway

The prediction-driven engine answers a fundamental question: *"Does this conversation require the agent to grow?"* Most of the time, the answer is no — and the system is smart enough to recognize that without asking an LLM. When growth *is* needed, it can be patient (Deferred GVU) or urgent (emergency loop), and it never forgets its past mistakes (MistakeNotebook).
