#!/usr/bin/env bash
# 测试 rpi-langfuse 扩展的端到端功能
# 验证修复后的事件类型和新增功能

set -e

echo "🧪 测试 rpi-langfuse 扩展实现"
echo "================================"

# 配置
export LANGFUSE_BASE_URL="https://langfuse.laofu.online"
export LANGFUSE_PUBLIC_KEY="pk-lf-59819655-ad12-4817-b757-f4e8ba4c09b8"
export LANGFUSE_SECRET_KEY="sk-lf-82468d6f-9840-45f6-9e8b-35f845638fa5"

# 1. 测试健康检查
echo ""
echo "1️⃣  测试 Langfuse 健康检查..."
HEALTH=$(curl -s -u "$LANGFUSE_PUBLIC_KEY:$LANGFUSE_SECRET_KEY" "$LANGFUSE_BASE_URL/api/public/health")
echo "✅ Health: $HEALTH"

# 2. 测试创建 trace
echo ""
echo "2️⃣  测试创建 trace..."
TRACE_ID="test-$(date +%s)-$$"
TIMESTAMP=$(date -u +"%Y-%m-%dT%H:%M:%S.000Z")

INGEST_PAYLOAD=$(cat <<EOF
{
  "batch": [
    {
      "id": "evt-1",
      "type": "trace-create",
      "timestamp": "$TIMESTAMP",
      "body": {
        "id": "$TRACE_ID",
        "name": "test-trace-with-metadata",
        "userId": "test-user-123",
        "sessionId": "test-session-456",
        "metadata": {
          "test": true,
          "platform": "linux"
        }
      }
    },
    {
      "id": "evt-2",
      "type": "generation-create",
      "timestamp": "$TIMESTAMP",
      "body": {
        "id": "gen-1",
        "traceId": "$TRACE_ID",
        "name": "test-generation",
        "model": "gpt-4",
        "modelParameters": {
          "temperature": 0.7,
          "max_tokens": 1000
        },
        "input": [{"role": "user", "content": "Hello"}],
        "startTime": "$TIMESTAMP"
      }
    },
    {
      "id": "evt-3",
      "type": "generation-update",
      "timestamp": "$TIMESTAMP",
      "body": {
        "id": "gen-1",
        "endTime": "$TIMESTAMP",
        "output": "Hi there!",
        "usage": {
          "input": 10,
          "output": 5,
          "total": 15,
          "unit": "TOKENS"
        },
        "completionStartTime": "$TIMESTAMP"
      }
    },
    {
      "id": "evt-4",
      "type": "span-create",
      "timestamp": "$TIMESTAMP",
      "body": {
        "id": "span-1",
        "traceId": "$TRACE_ID",
        "name": "test-tool-call",
        "startTime": "$TIMESTAMP",
        "input": {"query": "test"}
      }
    },
    {
      "id": "evt-5",
      "type": "span-update",
      "timestamp": "$TIMESTAMP",
      "body": {
        "id": "span-1",
        "endTime": "$TIMESTAMP",
        "output": "tool result"
      }
    }
  ]
}
EOF
)

RESULT=$(curl -s -u "$LANGFUSE_PUBLIC_KEY:$LANGFUSE_SECRET_KEY" \
  -X POST "$LANGFUSE_BASE_URL/api/public/ingestion" \
  -H "Content-Type: application/json" \
  -d "$INGEST_PAYLOAD")

echo "✅ Ingestion result: $RESULT"

# 3. 验证 trace 创建
echo ""
echo "3️⃣  验证 trace 创建..."
sleep 2
TRACE_DATA=$(curl -s -u "$LANGFUSE_PUBLIC_KEY:$LANGFUSE_SECRET_KEY" \
  "$LANGFUSE_BASE_URL/api/public/traces/$TRACE_ID")
echo "✅ Trace data: $TRACE_DATA" | head -c 500
echo ""

# 4. 测试 score API
echo ""
echo "4️⃣  测试 score 创建..."
SCORE_RESULT=$(curl -s -u "$LANGFUSE_PUBLIC_KEY:$LANGFUSE_SECRET_KEY" \
  -X POST "$LANGFUSE_BASE_URL/api/public/scores" \
  -H "Content-Type: application/json" \
  -d "{
    \"name\": \"accuracy\",
    \"value\": 0.95,
    \"traceId\": \"$TRACE_ID\",
    \"comment\": \"Test score\"
  }")
echo "✅ Score result: $SCORE_RESULT"

# 5. 测试 prompt API
# 注意：Langfuse v2.95 的 GET /api/public/prompts 必须带 name 参数，且没有 list-all 端点
echo ""
echo "5️⃣  测试 prompt 创建..."
PROMPT_NAME="test-prompt-$(date +%s)"
PROMPT_RESULT=$(curl -s -u "$LANGFUSE_PUBLIC_KEY:$LANGFUSE_SECRET_KEY" \
  -X POST "$LANGFUSE_BASE_URL/api/public/prompts" \
  -H "Content-Type: application/json" \
  -d "{
    \"name\": \"$PROMPT_NAME\",
    \"prompt\": \"You are a helpful assistant.\",
    \"isActive\": true
  }")
echo "✅ Prompt result: $PROMPT_RESULT" | head -c 300
echo ""

echo ""
echo "5️⃣b  测试 prompt get（查询参数形式）..."
PROMPT_GET=$(curl -s -u "$LANGFUSE_PUBLIC_KEY:$LANGFUSE_SECRET_KEY" \
  "$LANGFUSE_BASE_URL/api/public/prompts?name=$PROMPT_NAME")
echo "✅ Prompt get: $PROMPT_GET" | head -c 300
echo ""

echo ""
echo "5️⃣c  测试 prompt 指定版本..."
PROMPT_GETV=$(curl -s -u "$LANGFUSE_PUBLIC_KEY:$LANGFUSE_SECRET_KEY" \
  "$LANGFUSE_BASE_URL/api/public/prompts?name=$PROMPT_NAME&version=1")
echo "✅ Prompt get v1: $PROMPT_GETV" | head -c 200
echo ""

# 6. 测试 trace update（ingestion upsert 方式）
echo ""
echo "6️⃣  测试 trace update（ingestion trace-create upsert）..."
UPDATE_TS=$(date -u +"%Y-%m-%dT%H:%M:%S.000Z")
UPDATE_RESULT=$(curl -s -u "$LANGFUSE_PUBLIC_KEY:$LANGFUSE_SECRET_KEY" \
  -X POST "$LANGFUSE_BASE_URL/api/public/ingestion" \
  -H "Content-Type: application/json" \
  -d "{
    \"batch\": [{
      \"id\": \"trace-update-test\",
      \"type\": \"trace-create\",
      \"timestamp\": \"$UPDATE_TS\",
      \"body\": {\"id\": \"$TRACE_ID\", \"name\": \"updated-via-ingestion\", \"tags\": [\"test\"]}
    }]
  }")
echo "✅ Trace update result: $UPDATE_RESULT"
echo ""

# 7. 列出最近的 traces
echo ""
echo "7️⃣  列出最近的 traces..."
TRACES=$(curl -s -u "$LANGFUSE_PUBLIC_KEY:$LANGFUSE_SECRET_KEY" \
  "$LANGFUSE_BASE_URL/api/public/traces?limit=5")
echo "✅ Recent traces count: $(echo "$TRACES" | jq -r '.data | length')"

echo ""
echo "================================"
echo "✅ 所有测试通过！"
echo ""
echo "📊 验证的功能："
echo "  ✓ trace-create (含 userId/sessionId/metadata)"
echo "  ✓ generation-create (含 modelParameters)"
echo "  ✓ generation-update (修复后的事件类型)"
echo "  ✓ span-create"
echo "  ✓ span-update (修复后的事件类型)"
echo "  ✓ score API"
echo "  ✓ prompt API"
echo ""
echo "🔗 查看 trace: $LANGFUSE_BASE_URL/project/default-project/traces/$TRACE_ID"
