# Model Context Protocol (MCP) Monorepo

Collection of custom, high-performance MCP servers built in Rust.

## Servers

### [edge-tts-mcp](./edge-tts-mcp)
A fast, lightweight Model Context Protocol server that enables LLM agents to speak aloud using Microsoft Edge's Neural Text-to-Speech (`edge-tts`).

#### Features
- **Markdown Cleanup**: Automatically strips code blocks, headers, pipes, tables, and latex before synthesizing.
- **Media Player Control**: Pauses active music (`playerctl`) during narration and resumes when finished.
- **Hardware Routing**: Direct hardware audio sink support with buffer flush padding.
- **Stop Control**: Instant interrupt tool (`stop`).
- **Hands-Free Guard**: Built-in prompt instructions preventing unsolicited speech calls.
