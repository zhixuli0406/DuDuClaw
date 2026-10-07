import { cn } from '@/lib/utils';
import { Select, SelectTrigger, SelectValue, SelectContent, SelectItem } from '@/components/mds';

export type AgentLite = { name: string; display_name: string };

/** Small agent picker (MDS Select) shared by the MCP add/import/install/connect dialogs. */
export function AgentSelect({
  value,
  onChange,
  agents,
  placeholder,
  className,
}: {
  value: string;
  onChange: (v: string) => void;
  agents: ReadonlyArray<AgentLite>;
  placeholder?: string;
  className?: string;
}) {
  const current = agents.find((a) => a.name === value);
  return (
    <Select value={value} onValueChange={(v) => onChange(String(v))}>
      <SelectTrigger className={cn('w-full', className)}>
        <SelectValue placeholder={placeholder}>
          {current ? current.display_name || current.name : placeholder}
        </SelectValue>
      </SelectTrigger>
      <SelectContent>
        {agents.map((a) => (
          <SelectItem key={a.name} value={a.name}>
            {a.display_name || a.name}
          </SelectItem>
        ))}
      </SelectContent>
    </Select>
  );
}
