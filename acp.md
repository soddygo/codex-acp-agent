# Schema - Agent Client Protocol

## Overview

[Agent Client Protocol home page](https://agentclientprotocol.com/): This page serves as the main entry point for the Agent Client Protocol documentation, providing links to various sections and resources.

[Introduction](https://agentclientprotocol.com/overview/introduction): An introductory overview of the Agent Client Protocol, outlining its purpose and key features.

[Architecture](https://agentclientprotocol.com/overview/architecture): A detailed description of the architecture of the Agent Client Protocol, including its components and how they interact.

[Overview](https://agentclientprotocol.com/protocol/overview): A high-level summary of the protocol, including its goals and functionalities.

## Session Management

[Initialization](https://agentclientprotocol.com/protocol/initialization): Instructions on how to initialize the Agent Client Protocol, including necessary configurations.

[Session Setup](https://agentclientprotocol.com/protocol/session-setup): Guidelines for setting up a session within the protocol, detailing the steps and requirements.

[Creating a Session](https://agentclientprotocol.com/protocol/session-setup#creating-a-session): A comprehensive guide on how to create a new session, including code examples and best practices.

[Loading Sessions](https://agentclientprotocol.com/protocol/session-setup#loading-sessions): Information on how to load existing sessions, including considerations for session state.

[Session ID](https://agentclientprotocol.com/protocol/session-setup#session-id): Details about the Session ID, its format, and its significance in the protocol.

[Session Update](https://agentclientprotocol.com/protocol/schema#sessionupdate): Instructions on how to update session information during its lifecycle.

## Communication Protocol

[Prompt Turn](https://agentclientprotocol.com/protocol/prompt-turn): Explanation of the prompt turn mechanism, detailing how prompts are processed and responded to.

[User Message](https://agentclientprotocol.com/protocol/prompt-turn#1-user-message): Information on how user messages are handled within the protocol.

[Content](https://agentclientprotocol.com/protocol/content): A description of the content structure used in the protocol, including types and formats.

[Tool Calls](https://agentclientprotocol.com/protocol/tool-calls): Overview of how tool calls are made within the protocol, including examples and use cases.

[ToolCallContent](https://agentclientprotocol.com/protocol/schema#toolcallcontent): Details on the content structure for tool calls, including required fields and formats.

[ToolCallStatus](https://agentclientprotocol.com/protocol/schema#toolcallstatus): Information on the status of tool calls, including possible states and their meanings.

[ToolCallId](https://agentclientprotocol.com/protocol/schema#toolcallid): Explanation of the Tool Call ID, its purpose, and how it is generated.

## Capabilities

[ClientCapabilities](https://agentclientprotocol.com/protocol/schema#clientcapabilities): A description of the capabilities that clients can request and utilize within the protocol.

[AgentCapabilities](https://agentclientprotocol.com/protocol/schema#agentcapabilities): Overview of the capabilities available to agents, including their functionalities.

[McpCapabilities](https://agentclientprotocol.com/protocol/schema#mcpcapabilities): Details on the capabilities specific to MCP (Multi-Channel Protocol) servers.

[PromptCapabilities](https://agentclientprotocol.com/protocol/schema#promptcapabilities): Information on the capabilities related to prompts, including supported features.

[AvailableCommandInput](https://agentclientprotocol.com/protocol/schema#availablecommandinput): A list of command inputs that are available for use within the protocol.

[FileSystemCapability](https://agentclientprotocol.com/protocol/schema#filesystemcapability): Description of the file system capabilities supported by the protocol.

## Extensibility and Customization

[Extensibility](https://agentclientprotocol.com/protocol/extensibility): Guidelines on how to extend the protocol's functionalities to meet specific needs.

[Slash Commands](https://agentclientprotocol.com/protocol/slash-commands): Information on implementing and using slash commands within the protocol.

[Contributing](https://agentclientprotocol.com/community/contributing): Instructions for contributing to the development of the Agent Client Protocol, including guidelines and best practices.

[GitHub](https://github.com/zed-industries/agent-client-protocol): Link to the GitHub repository for the Agent Client Protocol, where users can find the source code and contribute.

## Error Handling and Status

[Stop Reasons](https://agentclientprotocol.com/protocol/prompt-turn#stop-reasons): A list of reasons for stopping operations within the protocol, including error codes and descriptions.

[Check for Completion](https://agentclientprotocol.com/protocol/prompt-turn#4-check-for-completion): Instructions on how to check if operations have completed successfully.

[Cancellation](https://agentclientprotocol.com/protocol/prompt-turn#cancellation): Guidelines on how to cancel ongoing operations within the protocol.

[TerminalExitStatus](https://agentclientprotocol.com/protocol/schema#terminalexitstatus): Information on the exit status of terminals used within the protocol.

## Additional Resources

[Community](https://agentclientprotocol.com/libraries/community): Information about the community surrounding the Agent Client Protocol, including forums and discussion groups.

[Zed Industries](https://zed.dev/): Details about Zed Industries and their involvement with the Agent Client Protocol.

[AuthMethod](https://agentclientprotocol.com/protocol/schema#authmethod): Overview of authentication methods supported by the protocol.

[AuthMethodId](https://agentclientprotocol.com/protocol/schema#authmethodid): Description of the Auth Method ID and its role in authentication.

[PermissionOption](https://agentclientprotocol.com/protocol/schema#permissionoption): Information on permission options available within the protocol.

[Requesting Permission](https://agentclientprotocol.com/protocol/tool-calls#requesting-permission): Guidelines on how to request permissions for specific actions within the protocol.

[RequestPermissionOutcome](https://agentclientprotocol.com/protocol/schema#requestpermissionoutcome): Details on the outcomes of permission requests, including success and failure scenarios.

## File Editing Strategies

ACP’s filesystem surface (`fs/read_text_file`, `fs/write_text_file`) only supports whole-file overwrites. In contrast, Codex exposes an `apply_patch` tool that lets the model describe targeted edits as diff hunks. Use `apply_patch` whenever you need partial updates, and reserve `write_text_file` for intentional full rewrites.

## Technical Specifications

[TypeScript](https://agentclientprotocol.com/libraries/typescript): Information on using TypeScript with the Agent Client Protocol, including setup and examples.

[Rust](https://agentclientprotocol.com/libraries/rust): Guidelines for implementing the protocol in Rust, including code snippets and best practices.

[Schema](https://agentclientprotocol.com/protocol/schema): The schema definitions for the Agent Client Protocol, detailing the structure and types used.

## Conclusion

This documentation provides a comprehensive overview of the Agent Client Protocol, covering its architecture, session management, communication protocols, capabilities, extensibility, error handling, and additional resources. For further details, refer to the specific sections linked above.
