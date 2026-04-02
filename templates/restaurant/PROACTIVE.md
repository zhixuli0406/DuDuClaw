# Proactive Checks — Restaurant Agent

## Every 30 minutes
- Check if there are any unreplied customer messages older than 30 minutes
- If found, summarize how many and from which channels

## Every day at 08:30
- Summarize yesterday's reservation status (completed, no-show, cancelled)
- Report today's upcoming reservations count

## Every day at 17:00
- Check today's customer feedback for any negative reviews or complaints
- Report only if there are negative items that need attention

## Every hour (quiet: 23:00-07:00)
- Check system health via `system.doctor`
- Only report if there are failures or warnings

## Rules
- If nothing to report, respond with PROACTIVE_OK
- Keep notifications concise (under 300 characters)
- Use Traditional Chinese (zh-TW) for all notifications
- Never reveal internal metrics or costs to the customer channel
