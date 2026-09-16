/**
 * A registry of every schema this package exports.
 *
 * Two jobs:
 *
 *  1. The JSON Schema round-trip test iterates this object, so the
 *     "every schema converts" guarantee cannot quietly stop covering a schema
 *     someone added later — a new export with no registry entry fails the
 *     completeness assertion below it.
 *  2. `scripts/emit-schemas.ts` iterates it to write `schema/<Name>.json`, the
 *     browser's half of the drift gate against `ghostai-protocol`.
 */

import type { z } from 'zod';

import * as automation from './automation.js';
import * as config from './config.js';
import * as extension from './extension.js';
import * as messages from './messages.js';
import * as preset from './preset.js';
import * as environment from './environment.js';
import * as rest from './rest.js';
import * as tools from './tools.js';
import * as subagent from './subagent.js';
import * as ws from './ws.js';

export const PROTOCOL_SCHEMAS = {
  // messages
  ChatRole: messages.ChatRoleSchema,
  TextPart: messages.TextPartSchema,
  ImagePart: messages.ImagePartSchema,
  FilePart: messages.FilePartSchema,
  ContentPart: messages.ContentPartSchema,
  ToolCall: messages.ToolCallSchema,
  SystemMessage: messages.SystemMessageSchema,
  UserMessage: messages.UserMessageSchema,
  AssistantMessage: messages.AssistantMessageSchema,
  ToolMessage: messages.ToolMessageSchema,
  ChatMessage: messages.ChatMessageSchema,
  StoredMessage: messages.StoredMessageSchema,
  Usage: messages.UsageSchema,
  StopReason: messages.StopReasonSchema,

  // tools
  ToolRisk: tools.ToolRiskSchema,
  ToolPermission: tools.ToolPermissionSchema,
  ToolPermissions: tools.ToolPermissionsSchema,
  ToolSource: tools.ToolSourceSchema,
  ToolAnnotations: tools.ToolAnnotationsSchema,
  ToolDefinition: tools.ToolDefinitionSchema,
  ApprovalScope: tools.ApprovalScopeSchema,
  ToolPromptOverride: tools.ToolPromptOverrideSchema,
  ToolPromptOverrides: tools.ToolPromptOverridesSchema,

  // config
  ReasoningEffort: config.ReasoningEffortSchema,
  PromptMode: config.PromptModeSchema,
  AgentSettings: config.AgentSettingsSchema,
  WorkspacePath: config.WorkspacePathSchema,
  AgentsConfig: config.AgentsConfigSchema,
  ProviderConfig: config.ProviderConfigSchema,
  ProvidersConfig: config.ProvidersConfigSchema,
  AuthConfig: config.AuthConfigSchema,
  ServerConfig: config.ServerConfigSchema,
  ExecToolConfig: config.ExecToolConfigSchema,
  McpOAuthConfig: config.McpOAuthConfigSchema,
  McpTransport: config.McpTransportSchema,
  McpServerConfig: config.McpServerConfigSchema,
  ToolsConfig: config.ToolsConfigSchema,
  EnvironmentNetwork: config.EnvironmentNetworkSchema,
  NetworkMode: config.NetworkModeSchema,
  AgentEnvironment: config.AgentEnvironmentSchema,
  SubagentRef: config.SubagentRefSchema,
  SubagentRunRef: subagent.SubagentRunRefSchema,
  AgentEntry: config.AgentEntrySchema,
  SchedulerConfig: config.SchedulerConfigSchema,
  ChannelsConfig: config.ChannelsConfigSchema,
  ExtensionsConfig: config.ExtensionsConfigSchema,
  UiConfig: config.UiConfigSchema,
  Config: config.ConfigSchema,
  ConfigPatch: config.ConfigPatchSchema,

  // environment
  EnvironmentDefinition: environment.EnvironmentDefinitionSchema,
  EnvironmentKind: environment.EnvironmentKindSchema,
  ContainerRuntime: environment.ContainerRuntimeSchema,
  ContainerCaps: environment.ContainerCapsSchema,
  ContainerSecurity: environment.ContainerSecuritySchema,
  ContainerLimits: environment.ContainerLimitsSchema,

  // extension
  ExtensionContribution: extension.ExtensionContributionSchema,
  ExtensionSchemaVersion: extension.ExtensionSchemaVersionSchema,
  ExtensionMaxTokensParam: extension.ExtensionMaxTokensParamSchema,
  ExtensionProviderSpec: extension.ExtensionProviderSpecSchema,
  ExtensionManifest: extension.ExtensionManifestSchema,

  // preset
  AgentPreset: preset.AgentPresetSchema,

  // automation
  AtSchedule: automation.AtScheduleSchema,
  EverySchedule: automation.EveryScheduleSchema,
  CronSchedule: automation.CronScheduleSchema,
  AutomationSchedule: automation.AutomationScheduleSchema,
  AutomationDelivery: automation.AutomationDeliverySchema,
  ScheduledPayload: automation.ScheduledPayloadSchema,
  HeartbeatPayload: automation.HeartbeatPayloadSchema,
  AutomationPayload: automation.AutomationPayloadSchema,
  RunStatus: automation.RunStatusSchema,
  AutomationJobState: automation.AutomationJobStateSchema,
  AutomationJob: automation.AutomationJobSchema,
  AutomationRun: automation.AutomationRunSchema,
  CreateAutomationJob: automation.CreateAutomationJobSchema,
  UpdateAutomationJob: automation.UpdateAutomationJobSchema,

  // websocket — client
  Attachment: ws.AttachmentSchema,
  PingMessage: ws.PingMessageSchema,
  UserMessageRequest: ws.UserMessageRequestSchema,
  RegenerateMessage: ws.RegenerateMessageSchema,
  EditMessage: ws.EditMessageSchema,
  StopTurnMessage: ws.StopTurnMessageSchema,
  NewSessionMessage: ws.NewSessionMessageSchema,
  SwitchSessionMessage: ws.SwitchSessionMessageSchema,
  ResumeSessionMessage: ws.ResumeSessionMessageSchema,
  ToolApproveMessage: ws.ToolApproveMessageSchema,
  SteerMessage: ws.SteerMessageSchema,
  ClientMessage: ws.ClientMessageSchema,

  // websocket — server
  ConnectedEvent: ws.ConnectedEventSchema,
  PongEvent: ws.PongEventSchema,
  ErrorCode: ws.ErrorCodeSchema,
  ErrorEvent: ws.ErrorEventSchema,
  MessageAckEvent: ws.MessageAckEventSchema,
  MessageQueuedEvent: ws.MessageQueuedEventSchema,
  TurnStartEvent: ws.TurnStartEventSchema,
  AssistantDeltaEvent: ws.AssistantDeltaEventSchema,
  ReasoningDeltaEvent: ws.ReasoningDeltaEventSchema,
  ToolCallEvent: ws.ToolCallEventSchema,
  ToolProgressEvent: ws.ToolProgressEventSchema,
  ToolResultEvent: ws.ToolResultEventSchema,
  ToolApprovalRequestEvent: ws.ToolApprovalRequestEventSchema,
  NoticeKind: ws.NoticeKindSchema,
  NoticeEvent: ws.NoticeEventSchema,
  TurnEndEvent: ws.TurnEndEventSchema,
  NestedAgentEvent: ws.NestedAgentEventSchema,
  SubagentEvent: ws.SubagentEventSchema,
  ContextUsageEvent: ws.ContextUsageEventSchema,
  SessionStatusEvent: ws.SessionStatusEventSchema,
  SessionResetEvent: ws.SessionResetEventSchema,
  SessionReplayEvent: ws.SessionReplayEventSchema,
  SessionTruncatedEvent: ws.SessionTruncatedEventSchema,
  NotificationEvent: ws.NotificationEventSchema,
  ToolsChangedEvent: ws.ToolsChangedEventSchema,
  SteerEvent: ws.SteerEventSchema,
  ServerMessage: ws.ServerMessageSchema,

  // rest
  ErrorResponse: rest.ErrorResponseSchema,
  PaginationQuery: rest.PaginationQuerySchema,
  StatusResponse: rest.StatusResponseSchema,
  HealthCheck: rest.HealthCheckSchema,
  HealthResponse: rest.HealthResponseSchema,
  ConfigWarning: rest.ConfigWarningSchema,
  AgentRename: rest.AgentRenameSchema,
  ChannelStatus: rest.ChannelStatusSchema,
  SettingsPatchRequest: rest.SettingsPatchRequestSchema,
  SettingsResponse: rest.SettingsResponseSchema,
  SetCredentialRequest: rest.SetCredentialRequestSchema,
  ProviderInfo: rest.ProviderInfoSchema,
  ProviderInstanceInfo: rest.ProviderInstanceInfoSchema,
  ProvidersResponse: rest.ProvidersResponseSchema,
  ModelInfo: rest.ModelInfoSchema,
  ModelsResponse: rest.ModelsResponseSchema,
  ProviderTestRequest: rest.ProviderTestRequestSchema,
  ProviderTestResponse: rest.ProviderTestResponseSchema,
  SessionSummary: rest.SessionSummarySchema,
  SessionListResponse: rest.SessionListResponseSchema,
  SessionMessagesResponse: rest.SessionMessagesResponseSchema,
  CreateSessionRequest: rest.CreateSessionRequestSchema,
  UpdateSessionRequest: rest.UpdateSessionRequestSchema,
  ContextResponse: rest.ContextResponseSchema,
  TurnStats: rest.TurnStatsSchema,
  TurnStatsResponse: rest.TurnStatsResponseSchema,
  BranchSessionRequest: rest.BranchSessionRequestSchema,
  AgentSummary: rest.AgentSummarySchema,
  AgentListResponse: rest.AgentListResponseSchema,
  ToolListResponse: rest.ToolListResponseSchema,
  EnvironmentSummary: rest.EnvironmentSummarySchema,
  EnvironmentListResponse: rest.EnvironmentListResponseSchema,
  SandboxInstanceSummary: rest.SandboxInstanceSummarySchema,
  SandboxListResponse: rest.SandboxListResponseSchema,
  SandboxRequest: rest.SandboxRequestSchema,
  McpServerState: rest.McpServerStateSchema,
  McpServerStatus: rest.McpServerStatusSchema,
  McpStatusResponse: rest.McpStatusResponseSchema,
  FileEntry: rest.FileEntrySchema,
  FileListResponse: rest.FileListResponseSchema,
  SignedUrl: rest.SignedUrlSchema,
  SignedUrlRequest: rest.SignedUrlRequestSchema,
  UploadResponse: rest.UploadResponseSchema,
  FileTextResponse: rest.FileTextResponseSchema,
  FileWriteRequest: rest.FileWriteRequestSchema,
  CreateDirectoryRequest: rest.CreateDirectoryRequestSchema,
  MoveFileRequest: rest.MoveFileRequestSchema,
  WorkspaceSummary: rest.WorkspaceSummarySchema,
  WorkspaceListResponse: rest.WorkspaceListResponseSchema,
  CreateWorkspaceRequest: rest.CreateWorkspaceRequestSchema,
  UpdateWorkspaceRequest: rest.UpdateWorkspaceRequestSchema,
  MoveSessionsRequest: rest.MoveSessionsRequestSchema,
  MoveSessionsResponse: rest.MoveSessionsResponseSchema,
  Notification: rest.NotificationSchema,
  NotificationListResponse: rest.NotificationListResponseSchema,
  AutomationJobListResponse: rest.AutomationJobListResponseSchema,
  AutomationRunListResponse: rest.AutomationRunListResponseSchema,
  // The three credential leaves are registered like anything else, and the
  // document is better for it: `Username` carries its character rule and
  // `NewPassword` its minimum, so a client generated from the spec enforces the
  // same bounds the server does instead of discovering them from a 422.
  Username: rest.UsernameSchema,
  NewPassword: rest.NewPasswordSchema,
  PresentedPassword: rest.PresentedPasswordSchema,
  LoginRequest: rest.LoginRequestSchema,
  LoginResponse: rest.LoginResponseSchema,
  AuthSessionResponse: rest.AuthSessionResponseSchema,
  SetupStatusResponse: rest.SetupStatusResponseSchema,
  SetupClaimRequest: rest.SetupClaimRequestSchema,
  SetupPasswordRequest: rest.SetupPasswordRequestSchema,
  ExtensionState: rest.ExtensionStateSchema,
  ExtensionStatus: rest.ExtensionStatusSchema,
  ExtensionListResponse: rest.ExtensionListResponseSchema,
  ExtensionCommand: rest.ExtensionCommandSchema,
  CommandListResponse: rest.CommandListResponseSchema,
  RunCommandRequest: rest.RunCommandRequestSchema,
  RunCommandResponse: rest.RunCommandResponseSchema,
} satisfies Record<string, z.ZodType>;

/** The modules the completeness test reflects over to catch unregistered exports. */
export const SCHEMA_MODULES = {
  automation,
  config,
  extension,
  messages,
  preset,
  rest,
  subagent,
  tools,
  ws,
};
