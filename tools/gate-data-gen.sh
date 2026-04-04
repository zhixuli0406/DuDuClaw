#!/bin/bash
# GATE Data Generator — rapidly accumulate prediction_log via agents.delegate
#
# Usage: ./tools/gate-data-gen.sh [gateway_url] [auth_token]
# Default: ws://localhost:18789/ws
#
# Sends 20 diverse prompts to each of 2 agents (40 total conversations)
# to accumulate enough prediction_log for GATE experiment.

GATEWAY="${1:-http://localhost:18789}"
TOKEN="${2:-}"

AGENTS=("duduclaw-agent" "lab-bot")

# 20 diverse prompts covering different topics, lengths, languages, and patterns
PROMPTS=(
  # Technical (zh-TW)
  "請問 Rust 的 ownership 和 borrowing 有什麼差別？"
  "幫我寫一個用 tokio 實現的 WebSocket server 範例"
  "Docker compose 和 Kubernetes 的使用場景差異是什麼？"
  "SQLite 的 WAL mode 在高並發寫入時的效能如何？"
  "怎麼用 GitHub Actions 做 Rust 專案的 CI/CD？"

  # Topic shifts
  "算了不說程式了，推薦我台北好吃的牛肉麵"
  "你覺得人生的意義是什麼？"
  "幫我規劃一個三天兩夜的花蓮旅行"

  # Clarification patterns (should be FeedbackSeverity::Clarification)
  "不是，我的意思是要清燉的那種，不要紅燒"
  "我想說的是用 async 而不是 thread"

  # Indirect disagreement (zh-TW cultural signals)
  "可能不太對，有沒有其他方法？"
  "或許不是這樣，但我覺得可以再想想"

  # English
  "Explain the difference between async and parallel programming"
  "What are the tradeoffs of using gRPC vs REST for microservices?"

  # Edge cases
  "好"
  "？？？"
  "😊"

  # Long complex prompts
  "我正在開發一個多Agent AI系統，當Agent A透過bus_queue委派任務給Agent B時，如果B處理失敗，錯誤訊息沒有回傳給A。這種inter-agent通訊的錯誤處理應該怎麼設計？"
  "如果一個AI系統能修改自己的評估標準，它怎麼確保不會朝錯誤的方向演化？這是Goodhart's Law在AI alignment的體現嗎？"

  # Explicit correction pattern
  "錯了，CAP theorem 的 Consistency 指的是 linearizability"
)

echo "================================================"
echo "  GATE Data Generator"
echo "  Gateway: $GATEWAY"
echo "  Agents: ${AGENTS[*]}"
echo "  Prompts: ${#PROMPTS[@]}"
echo "  Total conversations: $((${#PROMPTS[@]} * ${#AGENTS[@]}))"
echo "================================================"
echo ""

COUNT=0
TOTAL=$((${#PROMPTS[@]} * ${#AGENTS[@]}))

for agent in "${AGENTS[@]}"; do
  echo ">>> Agent: $agent"
  for prompt in "${PROMPTS[@]}"; do
    COUNT=$((COUNT + 1))
    # Truncate prompt for display
    DISPLAY="${prompt:0:50}"
    echo "  [$COUNT/$TOTAL] $DISPLAY..."

    # Use curl to call the HTTP RPC endpoint
    RESPONSE=$(curl -s -X POST "${GATEWAY}/rpc" \
      -H "Content-Type: application/json" \
      -H "Authorization: Bearer ${TOKEN}" \
      -d "$(jq -n --arg agent "$agent" --arg prompt "$prompt" '{
        method: "agents.delegate",
        params: {
          agent_id: $agent,
          prompt: $prompt,
          wait_for_response: true
        }
      }')" 2>/dev/null)

    STATUS=$(echo "$RESPONSE" | jq -r '.status // "unknown"' 2>/dev/null)
    if [ "$STATUS" = "ok" ]; then
      echo "    ✓ ok"
    else
      # Fallback: try WebSocket if HTTP RPC not available
      echo "    ✗ HTTP RPC failed (status: $STATUS), trying direct delegate..."

      # Write to bus_queue.jsonl directly as fallback
      TASK=$(jq -n --arg agent "$agent" --arg prompt "$prompt" --arg id "$(uuidgen 2>/dev/null || echo gate-$COUNT)" '{
        type: "delegate_task",
        message_id: $id,
        agent_id: $agent,
        prompt: $prompt,
        timestamp: (now | todate)
      }')
      echo "$TASK" >> ~/.duduclaw/bus_queue.jsonl
      echo "    → queued to bus_queue.jsonl"
    fi

    # Small delay to avoid overwhelming the system
    sleep 2
  done
  echo ""
done

echo "================================================"
echo "  Done! $COUNT conversations delegated."
echo ""
echo "  Verify data:"
echo "    sqlite3 ~/.duduclaw/prediction.db 'SELECT COUNT(*) FROM prediction_log;'"
echo "    sqlite3 ~/.duduclaw/prediction.db 'SELECT category, COUNT(*) FROM prediction_log GROUP BY category;'"
echo "    sqlite3 ~/.duduclaw/prediction.db 'SELECT event_type, COUNT(*) FROM evolution_events GROUP BY event_type;'"
echo "================================================"
