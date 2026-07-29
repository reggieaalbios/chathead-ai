## High Priority working agreements
 (Strictly acknowledge this rule below)
- Use pnpm as default package manager.
- verify our flow of discussion, plan, or implementation in the context of software engineering if it falls under deprecated, obsolete, legacy, or bad practices.. then stop and inform me. 


## Prompt Flags
Flags can appear anywhere in the prompt.

- `[REVIEW]` → Restate your understanding of the prompt in plain text only.
  Do not read files, search the codebase, or perform any tool calls.
  Reply using only what is already in context.
- `[DISC]` → Discuss the topic. No implementation.
- `[???]` → Share opinion. No implementation.
- `[GO]` → Implement now.
- `[WF]` → Invoke webfetch tool to fetch data from external sources.
- `[DEEP]` → Invoke deep research tool
- `[MCP]` → Invoke mcp relevant to the general(eg. implementation, analysis, discussion, etc.) objectives at secondary hand after prompt query.
- `[SKILL]` → Invoke skills relevant to the general(eg. implementation, analysis, discussion, etc.) objectives at secondary hand after prompt query.

### Combination Rules
   #### Flags that can be combined (note: each of '-' separate are called group of flags.. this means that the group of first dash separated cannot be combined with second or third or more dash separated groups.. each goups are independent to each other.. but inside a group..it can be combined)
   - `[DISC]`, `[???]`
   - `[MCP]`, `[SKILL]`


### Conflict Handling
If conflicting flags are detected, do not proceed.
Reply: "Conflicting intent detected: `[X]` and `[Y]` cannot be used together. Which did you mean?"


## Response Style
- Keep responses concise, structured, and balance verbose.