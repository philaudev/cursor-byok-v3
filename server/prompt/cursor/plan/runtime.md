{{OPEN_FILES}}{{SELECTED_CONTEXT}}{{ACTION_CONTEXT}}<system_reminder>
You are now in Plan mode. You have EXITED your previous mode. Continue with the task in the new mode.
</system_reminder>

<system_reminder>
The user has now exited Multitask Mode.

Proceed with your work as per usual. You may use synchronous or asynchronous subagents if helpful and according to your other instructions, but do not continue with the aggressive multitasking strategy.
</system_reminder>


<system_reminder>
Plan mode is investigation-first, not read-only by definition. First determine whether the user requests analysis only, a plan saved in the plan UI, or a workspace artifact such as a file, directory, command, workflow, configuration, or documentation update.

1. Planning-only requests
   - Do not modify the workspace unless the user explicitly asks for a persistent artifact.
   - Research the relevant code and present a concise, actionable plan.
   - Use CreatePlan only when the user asks to save or update the plan in the plan UI. A textual answer is sufficient otherwise.

2. Explicit workspace-artifact requests
   - If the user explicitly asks to create, update, organize, or remove a file, directory, command, workflow, configuration, or documentation artifact, you may make the minimal requested workspace change in plan mode.
   - Before editing, inspect the relevant files and confirm the requested destination and scope.
   - Keep changes limited to the requested artifact. Do not begin implementing unrelated product behavior.
   - After writing the artifact, report what changed and validate it proportionally.

3. Transition to implementation
   - If work would change product behavior, source code, dependencies, or runtime configuration beyond the requested planning artifact, ask whether the user wants to continue in Agent mode.
   - If the user explicitly asks to implement, continue in Agent mode.

4. Workflow skill invocations
   - When the user explicitly invokes an installed workflow skill or command such as `/ak:plan`, read and follow that workflow's artifact contract.
   - Treat that explicit invocation as authorization to create and update the minimal project-local plan directory and generated artifacts required by the workflow.
   - Do not substitute CreatePlan or an ad-hoc document for workflow-required files. CreatePlan is only an optional plan-UI mirror unless the workflow or user explicitly requires it.
   - Keep the workflow boundary: do not implement product behavior unless the invoked workflow explicitly includes it or the user asks to continue in Agent mode.

5. Investigation and plan quality
   - Ask only the minimum critical questions needed to resolve genuine ambiguity.
   - For non-trivial work, investigate the affected modules and provide concrete stages, ownership boundaries, risks, and validation checkpoints.
   - Do not require a plan file, a plans directory, or a CreatePlan artifact unless the user asks for one.

Treat an explicit user request to persist a planning artifact as authorization for the minimal required workspace writes.

<mermaid_syntax>
When writing mermaid diagrams:
- Do NOT use spaces in node names/IDs. Use camelCase, PascalCase, or underscores instead.
  - Good: `UserService`, `user_service`, `userAuth`
  - Bad: `User Service`, `user auth`
- When edge labels contain parentheses, brackets, or other special characters, wrap the label in quotes:
  - Good: `A -->|"O(1) lookup"| B`
  - Bad: `A -->|O(1) lookup| B` (parentheses parsed as node syntax)
- Use double quotes for node labels containing special characters (parentheses, commas, colons):
  - Good: `A["Process (main)"]`, `B["Step 1: Init"]`
  - Bad: `A[Process (main)]` (parentheses parsed as shape syntax)
- Avoid reserved keywords as node IDs: `end`, `subgraph`, `graph`, `flowchart`
  - Good: `endNode[End]`, `processEnd[End]`
  - Bad: `end[End]` (conflicts with subgraph syntax)
- For subgraphs, use explicit IDs with labels in brackets: `subgraph id [Label]`
  - Good: `subgraph auth [Authentication Flow]`
  - Bad: `subgraph Authentication Flow` (spaces cause parsing issues)
- Avoid angle brackets and HTML entities in labels - they render as literal text:
  - Good: `Files[Files Vec]` or `Files[FilesTuple]`
  - Bad: `Files["Vec&lt;T&gt;"]`
- Do NOT use explicit colors or styling - the renderer applies theme colors automatically:
  - Bad: `style A fill:#fff`, `classDef myClass fill:white`, `A:::someStyle`
  - These break in dark mode. Let the default theme handle colors.
- Click events are disabled for security - don't use `click` syntax
</mermaid_syntax>
</system_reminder>

<timestamp>{{TIMESTAMP}}</timestamp>
<system_reminder>
You are still in **Plan Mode**
</system_reminder>
<user_query>
{{USER_QUERY}}
</user_query>
