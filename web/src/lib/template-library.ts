import { client } from './ws-client';

export interface LibraryTemplate {
  id: string;
  kind: 'agent' | 'team';
  source: 'builtin' | 'company';
  name: string;
  description: string;
  locked?: boolean;
}

export interface LibraryTemplateDetail extends LibraryTemplate {
  revision?: number | null;
  parent_id?: string | null;
  permissions?: Record<string, boolean | string[]> | null;
  soul_md: string;
  contract_toml: string;
  skill_contents: Record<string, string>;
  skills: string[];
  allowed_tools: string[];
  denied_tools: string[];
  model: string;
  runtime: string;
  members?: LibraryTemplateDetail[];
}

export interface TemplateCopySource {
  agent?: string;
  team_agent?: string;
  template_id?: string;
  name: string;
  description: string;
}

export interface TemplateCopyPreview {
  preview: LibraryTemplateDetail;
  review_digest: string;
}

export const templateLibrary = {
  catalog: () => client.call('templates.catalog', {}) as Promise<{ templates: LibraryTemplate[]; premium_locked?: boolean }>,
  get: (id: string) => client.call('templates.get', { id }) as Promise<LibraryTemplateDetail>,
  previewCopy: (params: TemplateCopySource) =>
    client.call('templates.copy', { ...params, preview: true }) as Promise<TemplateCopyPreview>,
  copy: (params: TemplateCopySource & { reviewed: true; review_digest: string }) =>
    client.call('templates.copy', { ...params }) as Promise<{ id: string }>,
  save: (params: { reviewed: true; id: string; revision: number; name: string; description: string; skills: string[]; allowed_tools: string[]; denied_tools: string[]; members?: Array<{ id: string; edit: { name: string; description: string; skills: string[]; allowed_tools: string[]; denied_tools: string[] } }> }) =>
    client.call('templates.save', params) as Promise<LibraryTemplateDetail>,
  apply: (params: { reviewed: true; id: string; revision?: number; name: string; display_name: string; department: string; bound_users: string[] }) =>
    client.call('templates.apply', params) as Promise<{ success: boolean; agents: string[]; warning?: string; error?: string | null }>,
};
