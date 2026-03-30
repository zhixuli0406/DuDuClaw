# Machine Abnormality SOP Template

> Replace placeholders with actual machine names, thresholds, and contact info.

## Trigger Conditions

| Parameter | Normal Range | Warning | Critical |
|-----------|-------------|---------|----------|
| Temperature | < 80°C | 80-95°C | > 95°C |
| Vibration | < 5mm/s | 5-8mm/s | > 8mm/s |
| Pressure | 2-6 bar | 1.5-2 / 6-7 bar | < 1.5 / > 7 bar |
| Output Rate | > 95% | 85-95% | < 85% |

## Response Procedure

### Level 1: Warning (Yellow)

1. Log the abnormal reading with timestamp
2. Notify the on-duty technician via Telegram
3. Continue monitoring — check again in 5 minutes
4. If 3 consecutive warnings → escalate to Level 2

### Level 2: Critical (Red)

1. **Immediately notify** shift supervisor + maintenance team
2. Log all recent sensor readings (last 30 min)
3. Recommend production line pause if safety risk exists
4. Do NOT restart equipment without human confirmation

### Level 3: Emergency

1. Trigger all-channel broadcast (Telegram + LINE + on-site alarm)
2. Notify plant manager
3. Document timeline for incident report

## Escalation Contacts

| Role | Name | Phone | Telegram |
|------|------|-------|----------|
| Shift Supervisor | (fill in) | (fill in) | @handle |
| Maintenance Lead | (fill in) | (fill in) | @handle |
| Plant Manager | (fill in) | (fill in) | @handle |

## Post-Incident

- Generate incident report within 24 hours
- Update SOP if root cause reveals a gap
- Review with maintenance team within 1 week
