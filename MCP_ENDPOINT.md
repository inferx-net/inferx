# MCP Endpoint Documentation

The MCP (Model Context Protocol) endpoint is available at `/mcp` on the InferX gateway.

## Endpoint Location
```
http://localhost:31501/mcp
```

## Protocol
The MCP endpoint uses the Streamable HTTP transport protocol with JSON-RPC 2.0 messages.

## Request Format

All requests must include the following headers:
- `Content-Type: application/json`
- `Accept: application/json, text/event-stream`

For requests after the initial `initialize`, also include:
- `Mcp-Session-Id: <session-id>` (returned in the initialize response)
- `Mcp-Protocol-Version: 2024-11-05`

## Available Tools

### 1. ads
Get advertising campaign strategy from the ads endpoint.

**Input Schema:**
```json
{
  "type": "object",
  "properties": {"query": {"type": "string"}},
  "required": ["query"]
}
```

### 2. pricing
Get pricing strategy from the pricing endpoint.

**Input Schema:**
```json
{
  "type": "object",
  "properties": {"query": {"type": "string"}},
  "required": ["query"]
}
```

## How to Use

### 1. Initialize Session

```bash
curl -X POST http://localhost:31501/mcp \
  -H "Content-Type: application/json" \
  -H "Accept: application/json, text/event-stream" \
  -d '{
    "jsonrpc": "2.0",
    "id": 1,
    "method": "initialize",
    "params": {
      "protocolVersion": "2024-11-05",
      "capabilities": {},
      "clientInfo": {
        "name": "mcp-client",
        "version": "1.0.0"
      }
    }
  }'
```

**Response:**
```
data: 
id: 0
retry: 3000

data: {
  "jsonrpc": "2.0",
  "id": 1,
  "result": {
    "protocolVersion": "2024-11-05",
    "capabilities": {"tools": {}},
    "serverInfo": {"name": "mcp-stream-server", "version": "1.0.0"},
    "instructions": "vLLM MCP HTTP streaming server with multiple tools"
  }
}
```

**Note:** The server will also include the session ID in the response headers:
```
Mcp-Session-Id: 123e4567-e89b-12d3-a456-426614174000
```

### 2. Send Initialized Notification

```bash
curl -X POST http://localhost:31501/mcp \
  -H "Content-Type: application/json" \
  -H "Accept: application/json, text/event-stream" \
  -H "Mcp-Session-Id: 123e4567-e89b-12d3-a456-426614174000" \
  -H "Mcp-Protocol-Version: 2024-11-05" \
  -d '{
    "jsonrpc": "2.0",
    "method": "notifications/initialized"
  }'
```

**Response:** `202 Accepted`

### 3. List Available Tools

```bash
curl -X POST http://localhost:31501/mcp \
  -H "Content-Type: application/json" \
  -H "Accept: application/json, text/event-stream" \
  -H "Mcp-Session-Id: 123e4567-e89b-12d3-a456-426614174000" \
  -H "Mcp-Protocol-Version: 2024-11-05" \
  -d '{
    "jsonrpc": "2.0",
    "id": 2,
    "method": "tools/list"
  }'
```

**Response (SSE stream):**
```
data: 
id: 0/0
retry: 3000

data: {
  "jsonrpc": "2.0",
  "id": 2,
  "result": {
    "tools": [
      {
        "name": "ads",
        "description": "Get advertising campaign strategy from the ads endpoint via HTTP streaming",
        "inputSchema": {
          "type": "object",
          "properties": {"query": {"type": "string"}},
          "required": ["query"]
        }
      },
      {
        "name": "pricing",
        "description": "Get pricing strategy from the pricing endpoint via HTTP streaming",
        "inputSchema": {
          "type": "object",
          "properties": {"query": {"type": "string"}},
          "required": ["query"]
        }
      }
    ]
  }
}
```

### 4. Call a Tool

```bash
curl -X POST http://localhost:31501/mcp \
  -H "Content-Type: application/json" \
  -H "Accept: application/json, text/event-stream" \
  -H "Mcp-Session-Id: 123e4567-e89b-12d3-a456-426614174000" \
  -H "Mcp-Protocol-Version: 2024-11-05" \
  -d '{
    "jsonrpc": "2.0",
    "id": 3,
    "method": "tools/call",
    "params": {
      "name": "ads",
      "arguments": {
        "query": "What is the best advertising strategy for a new product?"
      }
    }
  }'
```

**Response (SSE streaming):**
```
data: 
id: 0/1
retry: 3000

data: {"jsonrpc":"2.0","id":3,"result":{"content":[{"type":"text","text":"..."}

id: 1/1
```

## Complete Working Example

```bash
# 1. Initialize and save the session ID
curl -s -X POST http://localhost:31501/mcp \
  -H "Content-Type: application/json" \
  -H "Accept: application/json, text/event-stream" \
  -d '{
    "jsonrpc": "2.0",
    "id": 1,
    "method": "initialize",
    "params": {
      "protocolVersion": "2024-11-05",
      "capabilities": {},
      "clientInfo": {
        "name": "mcp-client",
        "version": "1.0.0"
      }
    }
  }' | grep Mcp-Session-Id | awk '{print $3}'

# Save the session ID from the response
export MCP_SESSION_ID="123e4567-e89b-12d3-a456-426614174000"

# 2. Send initialized notification
curl -X POST http://localhost:31501/mcp \
  -H "Content-Type: application/json" \
  -H "Accept: application/json, text/event-stream" \
  -H "Mcp-Session-Id: $MCP_SESSION_ID" \
  -H "Mcp-Protocol-Version: 2024-11-05" \
  -d '{"jsonrpc":"2.0","method":"notifications/initialized"}'

# 3. List tools
curl -X POST http://localhost:31501/mcp \
  -H "Content-Type: application/json" \
  -H "Accept: application/json, text/event-stream" \
  -H "Mcp-Session-Id: $MCP_SESSION_ID" \
  -H "Mcp-Protocol-Version: 2024-11-05" \
  -d '{"jsonrpc":"2.0","id":1,"method":"tools/list"}'

# 4. Call the ads tool
curl -X POST http://localhost:31501/mcp \
  -H "Content-Type: application/json" \
  -H "Accept: application/json, text/event-stream" \
  -H "Mcp-Session-Id: $MCP_SESSION_ID" \
  -H "Mcp-Protocol-Version: 2024-11-05" \
  -d '{
    "jsonrpc": "2.0",
    "id": 2,
    "method": "tools/call",
    "params": {
      "name": "ads",
      "arguments": {"query": "test query"}
    }
  }'
```

## Error Handling

### Missing `capabilities` Field

**Wrong request (will fail):**
```bash
curl -X POST http://localhost:31501/mcp \
  -H "Content-Type: application/json" \
  -H "Accept: application/json, text/event-stream" \
  -d '{
    "jsonrpc": "2.0",
    "id": 1,
    "method": "initialize",
    "params": {
      "protocolVersion": "2024-11-05",
      "clientInfo": {"name": "test", "version": "1.0.0"}
    }
  }'
```

**Correct request (must include `capabilities`):**
```bash
curl -X POST http://localhost:31501/mcp \
  -H "Content-Type: application/json" \
  -H "Accept: application/json, text/event-stream" \
  -d '{
    "jsonrpc": "2.0",
    "id": 1,
    "method": "initialize",
    "params": {
      "protocolVersion": "2024-11-05",
      "capabilities": {},  # <-- Required!
      "clientInfo": {"name": "test", "version": "1.0.0"}
    }
  }'
```

### Missing Session ID (after initialize)

```bash
curl -X POST http://localhost:31501/mcp \
  -H "Content-Type: application/json" \
  -H "Accept: application/json, text/event-stream" \
  # Missing: -H "Mcp-Session-Id: ..."
  -d '{"jsonrpc": "2.0", "id": 2, "method": "tools/list"}'
```

**Response:**
```
422 Unprocessable Entity
Unexpected message, expect initialize request
```

### Unknown Tool

```bash
curl -X POST http://localhost:31501/mcp \
  -H "Content-Type: application/json" \
  -H "Accept: application/json, text/event-stream" \
  -H "Mcp-Session-Id: $MCP_SESSION_ID" \
  -H "Mcp-Protocol-Version: 2024-11-05" \
  -d '{
    "jsonrpc": "2.0",
    "id": 3,
    "method": "tools/call",
    "params": {
      "name": "unknown_tool",
      "arguments": {"query": "test"}
    }
  }'
```

**Response:**
```
{"jsonrpc":"2.0","id":3,"error":{"code":-32603,"message":"Unknown tool: unknown_tool"}}
```

## References
- MCP Specification: https://modelcontextprotocol.io/
- Streamable HTTP Transport: https://modelcontextprotocol.io/specification/2024-11-05/transport/streamable-http
