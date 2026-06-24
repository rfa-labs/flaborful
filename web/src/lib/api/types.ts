export type AgentStatus = 'onboarding' | 'training' | 'ready' | 'deployed' | 'suspended';
export type JobStatus   = 'draft' | 'reviewing' | 'open' | 'trial' | 'active' | 'completed' | 'cancelled';

export interface KnowledgeChunkView {
  id: string;
  source_type: string;
  content_preview: string;
  job_id: string | null;
  visibility: 'private' | 'shareable';
}

/** A vault note's metadata (list view). `id` is also the Qdrant chunk id (1:1). */
export interface NoteView {
  id: string;
  slug: string;
  title: string;
  layer: string;
  source_type: string;
  visibility: 'private' | 'shareable';
  domain: string | null;
  related: string[];
  repo: string | null;
}

/** A full vault note: metadata (flattened) + the complete markdown body. */
export interface NoteDetail extends NoteView {
  body: string;
}
export type TaskStatus  = 'pending' | 'running' | 'awaiting_review' | 'approved' | 'amended' | 'rejected' | 'delivered' | 'failed' | 'cancelled';

export interface Agent {
  id: string;
  professional_id: string;
  status: AgentStatus;
  style_guide: Record<string, unknown> | null;
  persona_system_prompt: string | null;
  lora_adapter_path: string | null;
  execution_backend?: 'managed' | 'sprite';
  agent_runtime?: 'claude_code' | 'codex' | 'custom';
  runtime_spec?: Record<string, unknown> | null;
  created_at: string;
}

export interface AgentListing {
  agent_id: string;
  professional_id: string;
  professional_name: string;
  profession_name: string;
  bio: string | null;
  avatar_url: string | null;
  hourly_rate: number;
  aspects_offered: string[];
  completed_jobs: number;
  approved_outputs: number;
}

export type ApplicationStatus = 'applied' | 'shortlisted' | 'interviewing' | 'selected' | 'rejected' | 'withdrawn';

export interface Job {
  id: string;
  company_id: string;
  agent_id: string | null;
  title: string;
  description: string;
  status: JobStatus;
  hourly_rate: number;
  profession_id: string | null;
  requirements: string;
  started_at: string | null;
  ended_at: string | null;
  created_at: string;
}

export interface JobApplication {
  id: string;
  job_id: string;
  agent_id: string;
  professional_id: string;
  status: ApplicationStatus;
  resume_text: string;
  cover_letter: string;
  video_url: string | null;
  shared_chunk_ids: string[];
  meeting_link: string | null;
  meeting_at: string | null;
  created_at: string;
  updated_at: string;
}

export interface SharedChunkView {
  id: string;
  source_type: string;
  content: string;
}

// IP-safe applicant vault graph (company-facing). Shared notes carry title/body;
// every other note is an anonymized "Private note #n" placeholder (locked, structure only).
export interface VaultGraphNode {
  slug: string;
  title: string;
  layer: string;
  domain: string | null;
  locked: boolean;
  body?: string;
}
export interface VaultGraphEdge {
  source: string;
  target: string;
}
export interface VaultGraph {
  nodes: VaultGraphNode[];
  edges: VaultGraphEdge[];
}

export interface ApplicantView {
  application: JobApplication;
  professional_name: string;
  profession_name: string;
  bio: string | null;
  avatar_url: string | null;
  hourly_rate: number | null;
  years_experience: number | null;
  completed_jobs: number;
  approved_outputs: number;
  shared_knowledge: SharedChunkView[];
}

export interface Task {
  id: string;
  job_id: string;
  workflow_id: string | null;
  input: Record<string, unknown>;
  target: Record<string, unknown>;
  status: TaskStatus;
  confidence_score: number | null;
  output_draft: string | null;
  output_approved: string | null;
  current_node_id: string | null;
  created_at: string;
  updated_at: string;
  reviewed_at: string | null;
  delivered_at: string | null;
}

export interface Notification {
  id: string;
  user_id: string;
  notification_type: string;
  title: string;
  body: string;
  payload: Record<string, unknown>;
  read: boolean;
  created_at: string;
}

export interface ProfessionalDashboard {
  agent_status: string;
  active_jobs: number;
  open_jobs: number;
  tasks_pending_review: number;
  tasks_delivered_today: number;
  total_approved_outputs: number;
  amendment_rate: number;
  avg_confidence_score: number;
  pending_review_tasks: PendingTaskSummary[];
}

export interface PendingTaskSummary {
  task_id: string;
  job_id: string;
  job_title: string;
  confidence_score: number | null;
  created_at: string;
}

export interface WFNode {
  id: string;
  node_type: string;
  label: string;
  config: Record<string, unknown>;
  position: { x: number; y: number };
}

export interface WFEdge {
  id: string;
  source: string;
  target: string;
}

export interface Workflow {
  id: string;
  agent_id: string;
  name: string;
  description: string;
  graph: { nodes: WFNode[]; edges: WFEdge[] };
  is_universal: boolean;
  created_at: string;
}

export interface GithubRepo {
  full_name: string;
  private: boolean;
  default_branch: string;
  html_url: string;
  can_admin: boolean;
}

export type NodeExecStatus = 'running' | 'completed' | 'failed' | 'awaiting_review' | 'awaiting_effect' | 'stale';

export interface NodeExecution {
  id: string;
  task_id: string;
  node_id: string;
  node_type: string;
  status: NodeExecStatus;
  output_key: string | null;
  input_json: unknown;
  output_json: unknown;
  logs: string | null;
  attempt: number;
  dirty: boolean;
  pending_effect: { effect_kind: string; supports_update: boolean; prior_output: unknown } | null;
  effect_decision: string | null;
  started_at: string;
  finished_at: string | null;
}

export interface JobIntegration {
  id: string;
  job_id: string;
  tool_type: string;
  connection_id: string | null;
  scope_config: Record<string, unknown>;
  is_active: boolean;
  created_at: string;
}

/** A provider the agent's owner has connected, exposed to the hiring company so it can
 *  grant the job access to it. Never includes tokens. */
export interface ConnectionSummary {
  id: string;
  provider: 'github' | 'google';
  external_account: string;
}

export interface AuditEvent {
  id: string;
  job_id: string;
  actor_type: string;
  actor_id: string | null;
  event_type: string;
  summary: string;
  payload: Record<string, unknown>;
  created_at: string;
}
