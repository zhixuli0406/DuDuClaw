import { useCallback, useEffect, useRef, useState } from 'react';
import { useIntl } from 'react-intl';
import {
  PawPrintIcon,
  UploadIcon,
  SparklesIcon,
  Trash2Icon,
  CheckIcon,
  DownloadIcon,
  MonitorIcon,
} from 'lucide-react';
import {
  Button,
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
  Input,
  Switch,
  Empty,
  Spinner,
  Badge,
  PageHeader,
} from '@/components/mds';
import { toast, formatError } from '@/lib/toast';
import {
  isTauri,
  petGenerate,
  petList,
  petActivate,
  petRemove,
  petModelStatus,
  petModelDownload,
  type PetSummary,
  type GeneratedPet,
  type ModelStatus,
} from '@/lib/pet';

/**
 * PetStudioPage — turn a photo into an interactive desktop pet (WP-P2/P3).
 *
 * Desktop-only: background removal + pack storage run in the Tauri shell, so a
 * plain browser shows a "desktop app only" empty state. The P0 flow is the
 * "original photo" mode — upload → local cutout → preview → name → activate.
 */

/** Read a File as a data URL (base64). */
function readAsDataUrl(file: File): Promise<string> {
  return new Promise((resolve, reject) => {
    const reader = new FileReader();
    reader.onload = () => resolve(String(reader.result));
    reader.onerror = () => reject(reader.error);
    reader.readAsDataURL(file);
  });
}

export function PetStudioPage() {
  const intl = useIntl();
  const t = (id: string, values?: Record<string, string | number>) =>
    intl.formatMessage({ id }, values);

  const [pets, setPets] = useState<PetSummary[]>([]);
  const [models, setModels] = useState<ModelStatus | null>(null);
  const [photoDataUrl, setPhotoDataUrl] = useState<string | null>(null);
  const [name, setName] = useState('');
  const [external, setExternal] = useState(false);
  const [generating, setGenerating] = useState(false);
  const [preview, setPreview] = useState<GeneratedPet | null>(null);
  const [downloading, setDownloading] = useState<'birefnet' | 'silueta' | null>(null);
  const fileRef = useRef<HTMLInputElement | null>(null);

  const desktop = isTauri();

  const refresh = useCallback(() => {
    if (!desktop) return;
    void petList().then(setPets).catch(() => setPets([]));
    void petModelStatus().then(setModels).catch(() => setModels(null));
  }, [desktop]);

  useEffect(() => {
    refresh();
  }, [refresh]);

  const onPickFile = useCallback(async (file: File | undefined) => {
    if (!file) return;
    try {
      const url = await readAsDataUrl(file);
      setPhotoDataUrl(url);
      setPreview(null);
      // Default the name from the filename (sans extension).
      setName((prev) => prev || file.name.replace(/\.[^.]+$/, ''));
    } catch (e) {
      toast.error(formatError(e));
    }
  }, []);

  const onGenerate = useCallback(async () => {
    if (!photoDataUrl) return;
    setGenerating(true);
    try {
      const result = await petGenerate(name.trim() || 'DuDu', photoDataUrl, external);
      setPreview(result);
      refresh();
      if (result.removerLabel === 'passthrough' && !external) {
        toast.info(t('pet.studio.noModelWarning'));
      }
    } catch (e) {
      toast.error(formatError(e));
    } finally {
      setGenerating(false);
    }
  }, [photoDataUrl, name, external, refresh, t]);

  const onActivate = useCallback(
    async (slug: string) => {
      try {
        await petActivate(slug);
        toast.success(t('pet.studio.activated'));
        refresh();
      } catch (e) {
        toast.error(formatError(e));
      }
    },
    [refresh, t]
  );

  const onDeactivate = useCallback(async () => {
    try {
      await petActivate(null);
      toast.success(t('pet.studio.dismissed'));
      refresh();
    } catch (e) {
      toast.error(formatError(e));
    }
  }, [refresh, t]);

  const onDelete = useCallback(
    async (slug: string) => {
      try {
        await petRemove(slug);
        refresh();
      } catch (e) {
        toast.error(formatError(e));
      }
    },
    [refresh]
  );

  const onDownloadModel = useCallback(
    async (variant: 'birefnet' | 'silueta') => {
      setDownloading(variant);
      try {
        await petModelDownload(variant);
        toast.success(t('pet.studio.modelDownloaded'));
        refresh();
      } catch (e) {
        toast.error(formatError(e));
      } finally {
        setDownloading(null);
      }
    },
    [refresh, t]
  );

  const header = (
    <PageHeader>
      <PawPrintIcon className="size-4 text-brand" />
      <span className="text-sm font-medium">{t('pet.studio.title')}</span>
    </PageHeader>
  );

  if (!desktop) {
    return (
      <div className="flex flex-1 flex-col">
        {header}
        <div className="grid flex-1 place-items-center p-6">
          <Empty
            icon={MonitorIcon}
            title={t('pet.studio.desktopOnlyTitle')}
            description={t('pet.studio.desktopOnlyBody')}
          />
        </div>
      </div>
    );
  }

  const noModel = models && !models.birefnetPresent && !models.siluetaPresent;

  return (
    <div className="flex flex-1 flex-col overflow-y-auto">
      {header}
      <div className="mx-auto w-full max-w-3xl space-y-5 p-4 sm:p-6">
        {/* Model setup hint — only when nothing is installed. */}
        {noModel && (
          <Card>
            <CardHeader>
              <CardTitle>{t('pet.studio.modelSetupTitle')}</CardTitle>
              <CardDescription>{t('pet.studio.modelSetupBody')}</CardDescription>
            </CardHeader>
            <CardContent className="flex flex-wrap gap-2">
              <Button
                variant="outline"
                disabled={downloading !== null}
                onClick={() => onDownloadModel('birefnet')}
              >
                {downloading === 'birefnet' ? (
                  <Spinner className="size-4" />
                ) : (
                  <DownloadIcon className="size-4" />
                )}
                {t('pet.studio.downloadBirefnet')}
              </Button>
              <Button
                variant="outline"
                disabled={downloading !== null}
                onClick={() => onDownloadModel('silueta')}
              >
                {downloading === 'silueta' ? (
                  <Spinner className="size-4" />
                ) : (
                  <DownloadIcon className="size-4" />
                )}
                {t('pet.studio.downloadSilueta')}
              </Button>
            </CardContent>
          </Card>
        )}

        {/* Upload + generate. */}
        <Card>
          <CardHeader>
            <CardTitle>{t('pet.studio.createTitle')}</CardTitle>
            <CardDescription>{t('pet.studio.createBody')}</CardDescription>
          </CardHeader>
          <CardContent className="space-y-4">
            <input
              ref={fileRef}
              type="file"
              accept="image/png,image/jpeg,image/webp"
              className="hidden"
              onChange={(e) => void onPickFile(e.target.files?.[0])}
            />

            <div className="grid grid-cols-2 gap-4">
              {/* Original */}
              <button
                type="button"
                onClick={() => fileRef.current?.click()}
                className="flex aspect-square items-center justify-center overflow-hidden rounded-xl border border-dashed border-surface-border bg-muted/30 transition-colors hover:bg-muted/50"
                aria-label={t('pet.studio.pickPhoto')}
              >
                {photoDataUrl ? (
                  <img
                    src={photoDataUrl}
                    alt={t('pet.studio.originalAlt')}
                    className="size-full object-contain"
                  />
                ) : (
                  <span className="flex flex-col items-center gap-2 text-muted-foreground">
                    <UploadIcon className="size-6" />
                    <span className="text-xs">{t('pet.studio.pickPhoto')}</span>
                  </span>
                )}
              </button>

              {/* Cutout preview */}
              <div className="grid aspect-square place-items-center overflow-hidden rounded-xl border border-surface-border bg-[repeating-conic-gradient(var(--color-muted)_0%_25%,transparent_0%_50%)] bg-[length:16px_16px]">
                {preview?.imageDataUrl ? (
                  <img
                    src={preview.imageDataUrl}
                    alt={t('pet.studio.cutoutAlt')}
                    className="size-full object-contain"
                  />
                ) : (
                  <span className="px-3 text-center text-xs text-muted-foreground">
                    {t('pet.studio.cutoutHint')}
                  </span>
                )}
              </div>
            </div>

            <div className="space-y-3">
              <Input
                value={name}
                onChange={(e) => setName(e.target.value)}
                placeholder={t('pet.studio.namePlaceholder')}
                maxLength={48}
              />
              <label className="flex items-center justify-between gap-3 text-sm">
                <span className="flex flex-col">
                  <span>{t('pet.studio.externalLabel')}</span>
                  <span className="text-xs text-muted-foreground">
                    {t('pet.studio.externalHint')}
                  </span>
                </span>
                <Switch checked={external} onCheckedChange={setExternal} />
              </label>
            </div>
          </CardContent>
          <div className="flex items-center justify-between gap-2 border-t border-surface-border px-6 py-4">
            <div className="text-xs text-muted-foreground">
              {preview && (
                <span>
                  {t('pet.studio.removerUsed', { remover: preview.removerLabel })}
                </span>
              )}
            </div>
            <div className="flex gap-2">
              <Button
                variant="brand"
                disabled={!photoDataUrl || generating}
                onClick={() => void onGenerate()}
              >
                {generating ? <Spinner className="size-4" /> : <SparklesIcon className="size-4" />}
                {t('pet.studio.generate')}
              </Button>
              {preview && (
                <Button variant="default" onClick={() => void onActivate(preview.slug)}>
                  <CheckIcon className="size-4" />
                  {t('pet.studio.activate')}
                </Button>
              )}
            </div>
          </div>
        </Card>

        {/* Existing pets. */}
        <Card>
          <CardHeader>
            <CardTitle>{t('pet.studio.myPetsTitle')}</CardTitle>
            <CardDescription>{t('pet.studio.myPetsBody')}</CardDescription>
          </CardHeader>
          <CardContent>
            {pets.length === 0 ? (
              <p className="py-6 text-center text-sm text-muted-foreground">
                {t('pet.studio.noPets')}
              </p>
            ) : (
              <ul className="divide-y divide-surface-border">
                {pets.map((p) => (
                  <li key={p.slug} className="flex items-center gap-3 py-3">
                    <PawPrintIcon className="size-4 text-muted-foreground" />
                    <span className="flex-1 truncate text-sm" title={p.displayName}>
                      {p.displayName}
                    </span>
                    {p.active && (
                      <>
                        <Badge variant="secondary">{t('pet.studio.activeBadge')}</Badge>
                        <Button variant="ghost" size="sm" onClick={() => void onDeactivate()}>
                          {t('pet.studio.dismiss')}
                        </Button>
                      </>
                    )}
                    {!p.active && (
                      <Button variant="ghost" size="sm" onClick={() => void onActivate(p.slug)}>
                        {t('pet.studio.use')}
                      </Button>
                    )}
                    <Button
                      variant="ghost"
                      size="icon-sm"
                      aria-label={t('pet.studio.delete')}
                      onClick={() => void onDelete(p.slug)}
                    >
                      <Trash2Icon className="size-4" />
                    </Button>
                  </li>
                ))}
              </ul>
            )}
          </CardContent>
        </Card>
      </div>
    </div>
  );
}
