// Node manifests drive the workflow builder's palette and every node's inline field UI.
// They are authored in the Rust backend (`src/services/node_registry.rs`) and fetched once via
// `GET /node-manifests`, so adding a node server-side automatically enriches the builder.

import { api } from './client';

export type FieldType =
  | 'text'
  | 'textarea'
  | 'number'
  | 'boolean'
  | 'enum'
  | 'file'
  | 'key_value'
  | 'repeater';

export type PortType = 'text' | 'list' | 'any';

export interface FieldSpec {
  key: string;
  label: string;
  type: FieldType;
  help?: string;
  placeholder?: string;
  required: boolean;
  default?: unknown;
  options?: string[];
  min?: number;
  max?: number;
  advanced: boolean;
  dynamic: boolean;
  /** [depKey, depValue] — only show this field when config[depKey] === depValue. */
  visible_when?: [string, string];
  /** Sub-fields for a `repeater`. */
  item?: FieldSpec[];
}

export interface PortSpec {
  key: string;
  label: string;
  type: PortType;
}

export type Category =
  | 'flow_basics'
  | 'using_ai'
  | 'utility'
  | 'mcp'
  | 'tools'
  | 'software_engineering'
  | 'work_execution';

export interface NodeManifest {
  type: string;
  category: Category;
  label: string;
  icon: string;
  color: string;
  help: string;
  loopable: boolean;
  /** Current manifest version for this node type; a stored node records the version it was created at. */
  version: number;
  inputs: FieldSpec[];
  outputs: PortSpec[];
}

/** Human-readable category labels for the palette, in display order. */
export const CATEGORY_LABELS: Record<Category, string> = {
  flow_basics: 'Flow Basics',
  using_ai: 'Using AI',
  utility: 'Utility',
  mcp: 'MCP',
  tools: 'Tools',
  software_engineering: 'Software Engineering',
  work_execution: 'Work Execution',
};

export const CATEGORY_ORDER: Category[] = [
  'flow_basics',
  'using_ai',
  'utility',
  'mcp',
  'tools',
  'software_engineering',
  'work_execution',
];

let cache: NodeManifest[] | null = null;

export async function loadManifests(): Promise<NodeManifest[]> {
  if (cache) return cache;
  cache = await api.get<NodeManifest[]>('/node-manifests');
  return cache;
}
