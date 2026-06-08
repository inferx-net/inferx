# Agent Gateway

A lightweight Rust-based API gateway that provides a session-based chat interface for pricing strategy conversations, powered by the Inferx pricing skill.

## Features

- **Session Management**: Create and manage multiple conversation sessions
- **Pricing Expertise**: Integrated with Inferx pricing skill for SaaS/GPU pricing advice
- **Session Compaction**: Automatic conversation summarization after 20+ messages using opencode compact prompt
- **Clean Output**: Returns model responses without thinking process markers
- **Web UI**: Built-in web interface at the root endpoint

## Quick Start

### Prerequisites

- Rust toolchain (for building)
- No additional dependencies required (binary is self-contained)

### Build

```bash
cd /home/brad/rust/inferx/test/agent_gateway
cargo build --release
```

### Run

**Development (foreground):**
```bash
./target/release/agent_gateway
```

**Production (background):**
```bash
./target/release/agent_gateway </dev/null >/tmp/agent_gateway.log 2>&1 &
```

**With nohup (persistent):**
```bash
nohup ./target/release/agent_gateway > /tmp/agent_gateway.log 2>&1 &
disown
```

## Web UI

The server includes a built-in web interface. After starting the server:

1. Open your browser and navigate to: `http://192.168.0.44:3000/` (or `http://localhost:3000/`)
2. The UI will automatically create a new session
3. Type your message and click "Send"

**Features:**
- Clean chat interface with user/assistant message styling
- Automatic session creation on load
- Non-streaming responses (simpler, faster)
- Works in any modern browser

**Note:** The web UI uses the same API endpoints as the curl commands above. You can use either interface interchangeably.

## API Endpoints

### Health Check
```bash
curl http://localhost:3000/health
```

### List Sessions
```bash
curl http://localhost:3000/sessions
```

### Create Session
```bash
curl -X POST http://localhost:3000/sessions
```

### Send Prompt
```bash
curl -X POST http://localhost:3000/sessions/{session_id}/prompt \
  -H "Content-Type: application/json" \
  -d '{"message":"What pricing model for SaaS GPU?","stream":false}'
```

### Get Session Details
```bash
curl http://localhost:3000/sessions/{session_id}
```

### Web UI
```bash
curl http://localhost:3000/
# Or open in browser: http://localhost:3000/
```

## Configuration

The service uses the following configuration (defined in `main.rs`):

- **Model Endpoint**: `https://dev4.inferx.net/skills/tn-a3t79iogb2/default/pricing/v1/chat/completions`
- **Model**: `Qwen/Qwen3.6-35B-A3B-FP8`
- **Compaction Trigger**: After 20+ messages (10+ exchanges)
- **Port**: `3000`
- **Compaction Threshold**: `> 6` messages (3+ exchanges)

## Session Compaction

When a session exceeds 20 messages, the system automatically compacts the conversation history using the opencode compact system prompt template:

```markdown
## Goal
- [single-sentence task summary]

## Constraints & Preferences
- [user constraints, preferences, specs, or "(none)"]

## Progress
### Done
- [completed work or "(none)"]

### In Progress
- [current work or "(none)"]

### Blocked
- [blockers or "(none)"]

## Key Decisions
- [decision and why, or "(none)"]

## Next Steps
- [ordered next actions or "(none)"]

## Critical Context
- [important technical facts, errors, open questions, or "(none)"]

## Relevant Files
- [file or directory path: why it matters, or "(none)"]
```

## Example Usage

```bash
# Create a session
SESSION=$(curl -s -X POST http://localhost:3000/sessions | jq -r '.session.id')

# Ask a pricing question
curl -X POST "http://localhost:3000/sessions/$SESSION/prompt" \
  -H "Content-Type: application/json" \
  -d '{"message":"What is a good pricing model for a SaaS GPU product?","stream":false}'

# Ask a follow-up
curl -X POST "http://localhost:3000/sessions/$SESSION/prompt" \
  -H "Content-Type: application/json" \
  -d '{"message":"Should I charge per GPU hour or per generation?","stream":false}'
```

## Logs

View logs:
```bash
tail -f /tmp/agent_gateway.log
```

## Troubleshooting

**Server not responding:**
```bash
# Check if process is running
ps aux | grep agent_gateway

# Check logs
cat /tmp/agent_gateway.log

# Restart
pkill agent_gateway
./target/release/agent_gateway </dev/null >/tmp/agent_gateway.log 2>&1 &
```

**Port already in use:**
```bash
# Find process using port 3000
lsof -i :3000

# Kill it
kill -9 <PID>
```

## Architecture

- **Framework**: Axum (Rust web framework)
- **HTTP Client**: Reqwest
- **Async Runtime**: Tokio
- **Logging**: Tracing
- **Data Serialization**: Serde + serde_json
