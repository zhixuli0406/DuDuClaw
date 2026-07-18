import { useState } from 'react';
import { useIntl } from 'react-intl';
import { api } from '@/lib/api';
import {
  OUTFIT_CATALOG,
  OUTFIT_SLOTS,
  defaultOutfitFor,
  parseOutfit,
  randomOutfit,
  type AgentOutfit,
  type OutfitSlot,
} from '@/lib/outfit';
import { cn } from '@/lib/utils';
import { toast, formatError } from '@/lib/toast';
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
  DialogFooter,
  DialogClose,
  Button,
} from '@/components/mds';
import { CharacterAvatar } from '@/components/character';
import { Dices, RotateCcw, Footprints } from 'lucide-react';

/**
 * WardrobeDialog（衣帽間）— dress an AI staff member from slot-based parts
 * (hat / head / body / hands / feet / accessory + tint). The live bust preview
 * IS the same `CharacterAvatar` every roster row renders, and the PixiJS world
 * draws the same slots — save once, look changes everywhere.
 */
export function WardrobeDialog({
  agentId,
  displayName,
  outfit,
  onClose,
  onSaved,
}: {
  agentId: string;
  displayName: string;
  /** The currently saved outfit (null = never dressed). */
  outfit: AgentOutfit | null;
  onClose: () => void;
  onSaved: () => void;
}) {
  const intl = useIntl();
  const [draft, setDraft] = useState<AgentOutfit>(
    () => parseOutfit(outfit) ?? defaultOutfitFor(agentId),
  );
  // True while the draft equals "no saved outfit" (save sends null → seeded).
  const [cleared, setCleared] = useState(outfit == null);
  const [saving, setSaving] = useState(false);

  const edit = (patch: Partial<AgentOutfit>) => {
    setDraft((d) => ({ ...d, ...patch }));
    setCleared(false);
  };

  const handleSave = async () => {
    setSaving(true);
    try {
      await api.agents.setOutfit(agentId, cleared ? null : draft);
      toast.success(intl.formatMessage({ id: 'wardrobe.saved' }));
      onSaved();
      onClose();
    } catch (e) {
      toast.error(formatError(e));
    } finally {
      setSaving(false);
    }
  };

  const slotLabel = (slot: OutfitSlot) => intl.formatMessage({ id: `wardrobe.slot.${slot}` });
  const itemLabel = (slot: OutfitSlot, item: string) =>
    item === ''
      ? intl.formatMessage({ id: 'wardrobe.item.none' })
      : intl.formatMessage({ id: `wardrobe.item.${slot}.${item}`, defaultMessage: item });

  return (
    <Dialog open onOpenChange={(o) => !o && onClose()}>
      <DialogContent className="sm:max-w-3xl">
        <DialogHeader>
          <DialogTitle>
            {`${displayName} · ${intl.formatMessage({ id: 'wardrobe.title' })}`}
          </DialogTitle>
        </DialogHeader>

        <div className="flex max-h-[70vh] flex-col gap-4 overflow-y-auto pr-1 sm:flex-row">
          {/* Live preview — the exact roster/world character. */}
          <div className="flex shrink-0 flex-col items-center gap-2 sm:sticky sm:top-0 sm:w-52">
            <div className="rounded-2xl bg-muted p-4">
              <CharacterAvatar
                agentId={agentId}
                name={displayName}
                size={168}
                variant="bust"
                pose="waving"
                avatar={null}
                outfit={cleared ? null : draft}
              />
            </div>
            <p className="flex items-center gap-1 text-center text-xs text-muted-foreground">
              <Footprints className="h-3.5 w-3.5 shrink-0" />
              {intl.formatMessage({ id: 'wardrobe.feetHint' })}
            </p>
            <div className="flex gap-2">
              <Button
                variant="secondary"
                size="sm"
                onClick={() => {
                  setDraft(randomOutfit());
                  setCleared(false);
                }}
              >
                <Dices />
                {intl.formatMessage({ id: 'wardrobe.random' })}
              </Button>
              <Button
                variant="ghost"
                size="sm"
                onClick={() => {
                  setDraft(defaultOutfitFor(agentId));
                  setCleared(true);
                }}
              >
                <RotateCcw />
                {intl.formatMessage({ id: 'wardrobe.reset' })}
              </Button>
            </div>
          </div>

          {/* Slot pickers */}
          <div className="min-w-0 flex-1 space-y-4">
            {/* Tint */}
            <div>
              <p className="mb-1.5 text-sm font-medium text-foreground">
                {intl.formatMessage({ id: 'wardrobe.tint' })}
              </p>
              <div className="flex flex-wrap gap-1.5">
                <button
                  type="button"
                  onClick={() => edit({ tint: 0 })}
                  className={cn(
                    'rounded-full border px-2.5 py-1 text-xs font-medium transition-colors',
                    draft.tint === 0
                      ? 'border-brand/40 bg-brand/10 text-brand ring-2 ring-brand/25'
                      : 'border-border text-muted-foreground hover:bg-muted',
                  )}
                >
                  {intl.formatMessage({ id: 'wardrobe.tint.auto' })}
                </button>
                {Array.from({ length: 10 }, (_, i) => i + 1).map((n) => (
                  <button
                    key={n}
                    type="button"
                    onClick={() => edit({ tint: n })}
                    aria-label={`tint ${n}`}
                    className={cn(
                      'h-7 w-7 rounded-full border-2 transition-transform',
                      draft.tint === n
                        ? 'scale-110 border-brand ring-2 ring-brand/25'
                        : 'border-transparent',
                    )}
                    style={{ background: `linear-gradient(180deg, var(--agent-${n}a), var(--agent-${n}b))` }}
                  />
                ))}
              </div>
            </div>

            {OUTFIT_SLOTS.map((slot) => (
              <div key={slot}>
                <p className="mb-1.5 text-sm font-medium text-foreground">{slotLabel(slot)}</p>
                <div className="flex flex-wrap gap-1.5">
                  {OUTFIT_CATALOG[slot].map((item) => {
                    const active = draft[slot] === item;
                    return (
                      <button
                        key={item || 'none'}
                        type="button"
                        onClick={() => edit({ [slot]: item } as Partial<AgentOutfit>)}
                        aria-pressed={active}
                        className={cn(
                          'rounded-full border px-3 py-1.5 text-xs font-medium transition-colors',
                          active
                            ? 'border-brand/50 bg-brand/10 text-brand ring-2 ring-brand/25'
                            : 'border-border text-muted-foreground hover:bg-muted',
                        )}
                      >
                        {itemLabel(slot, item)}
                      </button>
                    );
                  })}
                </div>
              </div>
            ))}
          </div>
        </div>

        <DialogFooter>
          <DialogClose
            render={<Button variant="outline">{intl.formatMessage({ id: 'common.cancel' })}</Button>}
          />
          <Button variant="brand" onClick={handleSave} disabled={saving}>
            {saving
              ? intl.formatMessage({ id: 'common.saving' })
              : intl.formatMessage({ id: 'common.save' })}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
