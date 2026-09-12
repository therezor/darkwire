/**
 * Editing one workspace text file.
 *
 * The panel opens **read-only**. That is the first decision and the one the
 * rest follows from: this tree is what an agent has been writing to, so the
 * common reason to open a file is to find out what a turn did, and a textarea
 * that is already live is one stray keystroke away from editing the evidence.
 * Editing is a mode you ask for.
 *
 * The other three:
 *
 *  - **A save carries the timestamp it loaded.** The agent writes to this tree
 *    while the dialog is open, so `PUT /api/files/text` refuses a save whose
 *    file moved and the panel offers to reload rather than clobbering a turn's
 *    work. Without it, "leave the editor open through a turn" silently deletes
 *    whatever that turn wrote.
 *  - **A truncated file cannot be edited at all.** The server answers with a
 *    prefix past its read limit, and saving a prefix deletes the rest of the
 *    file. There is no version of that worth offering, so the mode button is
 *    not there.
 *  - **Closing with unsaved edits asks first.** It is the only way to lose work
 *    here, and `Escape` and the overlay both do it by one keypress.
 */

import type { TFunction } from 'i18next';
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import { Eye, FileWarning, Pencil, RotateCw, Save } from 'lucide-react';
import {
  useEffect,
  useRef,
  useState,
  type JSX,
  type KeyboardEvent,
} from 'react';
import { useTranslation } from 'react-i18next';

import type { FileEntry } from '@ghostwire/protocol';

import { ApiError, api } from '@/lib/api.js';
import { formatBytes } from '@/lib/format.js';
import { queryKeys } from '@/lib/query.js';
import { Button } from '@/components/ui/button.js';
import { toast } from '@/components/ui/toast.js';
import { CodeEditor } from './code-editor.js';
import { languageForFile, parentOf } from './paths.js';

interface FileEditorProps {
  readonly entry: FileEntry;
  /** Which workspace `entry.path` is relative to. */
  readonly workspace: string;
  /** Raised whenever the buffer differs from what was loaded, so the dialog can guard the close. */
  readonly onDirtyChange?: (dirty: boolean) => void;
}

export function FileEditor({
  entry,
  workspace,
  onDirtyChange,
}: FileEditorProps): JSX.Element {
  const { t } = useTranslation();
  const queryClient = useQueryClient();
  const [draft, setDraft] = useState<string | undefined>(undefined);
  const [editing, setEditing] = useState(false);
  const [conflict, setConflict] = useState(false);
  const textarea = useRef<HTMLTextAreaElement>(null);

  const file = useQuery({
    queryKey: queryKeys.fileText(workspace, entry.path),
    queryFn: ({ signal }) => api.readText(workspace, entry.path, signal),
    // Always the file as it is now, never as it was when the dialog last
    // opened: a stale buffer is what the conflict check exists to catch, and
    // serving one from cache would manufacture the conflict it then reports.
    staleTime: 0,
    gcTime: 0,
  });

  const loaded = file.data;
  const content = draft ?? loaded?.content ?? '';
  const dirty = draft !== undefined && draft !== loaded?.content;

  useEffect(() => {
    onDirtyChange?.(dirty);
  }, [dirty, onDirtyChange]);

  const save = useMutation({
    mutationFn: async (text: string) => {
      if (loaded === undefined) throw new Error('The file is not loaded yet');
      return await api.writeText(
        workspace,
        entry.path,
        text,
        loaded.modifiedAtMs,
      );
    },
    onSuccess: (written) => {
      setConflict(false);
      setDraft(undefined);
      setEditing(false);
      // Both: the text query holds the new `modifiedAtMs` the next save has to
      // match, and the listing holds the size and time in the row behind this.
      void queryClient.invalidateQueries({
        queryKey: queryKeys.fileText(workspace, entry.path),
      });
      void queryClient.invalidateQueries({
        queryKey: queryKeys.files(workspace, parentOf(entry.path)),
      });
      toast.success(`Saved ${entry.name}`, formatBytes(written.sizeBytes));
    },
    onError: (error: Error) => {
      // 409 is not a failure to report and forget: the file moved, the edits
      // are still in the buffer, and the reader has a decision to make.
      if (error instanceof ApiError && error.status === 409) {
        setConflict(true);
        return;
      }
      toast.error('Could not save the file', error.message);
    },
  });

  /** Discards the buffer and re-reads. The only exit from a conflict. */
  const reload = (): void => {
    setDraft(undefined);
    setConflict(false);
    setEditing(false);
    void queryClient.invalidateQueries({
      queryKey: queryKeys.fileText(workspace, entry.path),
    });
  };

  if (file.isPending) {
    return <p className="file-preview__note">{t('files.reading')}</p>;
  }
  if (file.isError) {
    return (
      <p role="alert" className="page__error">
        {file.error.message}
      </p>
    );
  }

  const { truncated } = file.data;

  const handleKeyDown = (event: KeyboardEvent<HTMLTextAreaElement>): void => {
    // The shortcut every editor has. Without it the browser's own Save-page
    // dialog opens over the file being edited, which is the worst of both.
    if ((event.metaKey || event.ctrlKey) && event.key === 's') {
      event.preventDefault();
      if (dirty && !save.isPending) save.mutate(content);
    }
  };

  return (
    <div className="stack file-editor">
      <div className="cluster file-editor__toolbar">
        <span className="micro-label">
          {truncated
            ? 'Read-only — truncated'
            : dirty
              ? 'Unsaved changes'
              : editing
                ? 'Editing'
                : 'Read-only'}
        </span>
        <span className="spacer" />

        <Button
          variant="ghost"
          size="sm"
          onClick={reload}
          disabled={file.isFetching}
        >
          <RotateCw />
          Reload
        </Button>

        {!truncated && (
          <Button
            variant="ghost"
            size="sm"
            aria-pressed={editing}
            // Disabled rather than silently refused: leaving edit mode with a
            // dirty buffer would take the Save button away with it, and a
            // control that looks live and does nothing is worse than one that
            // says why it cannot.
            disabled={editing && dirty}
            {...(editing && dirty ? { title: 'Save or reload first' } : {})}
            onClick={() => {
              setEditing(!editing);
              if (!editing) {
                requestAnimationFrame(() => textarea.current?.focus());
              }
            }}
          >
            {editing ? <Eye /> : <Pencil />}
            {editing ? 'View' : 'Edit'}
          </Button>
        )}

        {editing && !truncated && (
          <Button
            variant="primary"
            size="sm"
            disabled={!dirty || save.isPending}
            onClick={() => {
              save.mutate(content);
            }}
          >
            <Save />
            {save.isPending ? 'Saving…' : 'Save'}
          </Button>
        )}
      </div>

      {truncated && (
        <p className="notice notice--warning">
          <FileWarning />
          <span>
            {t('files.tooBig', { size: formatBytes(entry.sizeBytes) })}
          </span>
        </p>
      )}

      {conflict && (
        <p role="alert" className="notice notice--danger">
          <FileWarning />
          <span>{t('files.conflict')}</span>
        </p>
      )}

      <CodeEditor
        value={content}
        readOnly={!editing || truncated}
        language={languageForFile(entry.name)}
        label={`Contents of ${entry.name}`}
        textareaRef={textarea}
        onChange={setDraft}
        onKeyDown={handleKeyDown}
      />

      <p className="micro-label">
        {languageForFile(entry.name) === ''
          ? 'plain text'
          : languageForFile(entry.name)}{' '}
        · {lineLabel(content, t)}
        {truncated ? ` of ${formatBytes(entry.sizeBytes)}` : ''}
      </p>
    </div>
  );
}

/** "1 line" / "412 lines", for the footer under the editor. */
function lineLabel(content: string, t: TFunction): string {
  return t('files.lines', { count: content.split('\n').length });
}
