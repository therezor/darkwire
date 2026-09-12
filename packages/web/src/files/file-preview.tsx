/**
 * Looking at one workspace file.
 *
 * Two paths out of here, and which one a file takes is decided in two different
 * places on purpose:
 *
 *  - **A picture is decided by the MIME type**, on the server, and rendered
 *    through a signed URL. `<img src>` cannot carry an `Authorization` header
 *    and cannot be relied on to carry a `SameSite=Strict` cookie, so the
 *    tempting fix is to make the file endpoint public — which is anonymous read
 *    access to a tree a language model writes to. `POST /api/files/signed-url`
 *    mints a short-lived HMAC token naming one path instead, and the endpoint
 *    stays authenticated.
 *  - **Everything else is offered to the editor**, which asks
 *    `GET /api/files/text` and lets the *bytes* decide. The server's MIME table
 *    is deliberately small, so guessing here would refuse `.py`, `.ts` and
 *    `.css` — the files most worth opening — for having no entry in it. A file
 *    that really is binary comes back as a refusal and lands on the download
 *    panel, one request later.
 *
 * Text is read into the page, never framed. The workspace holds model-authored
 * files; an `<iframe>` would execute one.
 */

import { useQuery } from '@tanstack/react-query';
import { Download, FileWarning } from 'lucide-react';
import type { JSX } from 'react';
import { useTranslation } from 'react-i18next';

import type { FileEntry } from '@ghostwire/protocol';

import { ApiError, api } from '@/lib/api.js';
import { formatBytes } from '@/lib/format.js';
import { queryKeys } from '@/lib/query.js';
import { Button } from '@/components/ui/button.js';
import { FileEditor } from './file-editor.js';
import { isImage } from './paths.js';

interface FilePreviewProps {
  /** Which workspace `entry.path` is relative to. */
  readonly workspace: string;
  readonly entry: FileEntry;
  readonly onDirtyChange?: (dirty: boolean) => void;
}

export function FilePreview({
  entry,
  workspace,
  onDirtyChange,
}: FilePreviewProps): JSX.Element {
  const { t } = useTranslation();
  const image = isImage(entry.mimeType);

  const signed = useQuery({
    queryKey: queryKeys.fileUrl(workspace, entry.path),
    queryFn: ({ signal }) => api.signUrl(workspace, entry.path, signal),
    // A URL that expired while the dialog was open is not a cached answer worth
    // keeping: the next open mints a fresh one.
    gcTime: 0,
  });

  /**
   * Whether the editor will take this file, asked once and cheaply.
   *
   * The editor runs the same query — same key, same client — so this is not a
   * second request: it is the same one, read here to decide which panel to
   * render and read there for its content.
   */
  const text = useQuery({
    queryKey: queryKeys.fileText(workspace, entry.path),
    queryFn: ({ signal }) => api.readText(workspace, entry.path, signal),
    enabled: !image,
    staleTime: 0,
    gcTime: 0,
    // A binary file is a 400 and is the answer, not a hiccup worth two retries.
    retry: false,
  });

  if (signed.isPending) {
    return <p className="file-preview__note">{t('files.preparingLink')}</p>;
  }
  if (signed.isError) {
    return (
      <p role="alert" className="page__error">
        {signed.error.message}
      </p>
    );
  }

  const { url } = signed.data;
  // Only a refusal to *read it as text* falls back to the download panel. A 403
  // or a 500 is a failure the editor should report as one, not a file quietly
  // reclassified as binary.
  const notText =
    text.isError && text.error instanceof ApiError && text.error.status === 400;

  return (
    <div className="stack file-preview">
      {image && (
        // Constrained by the viewport rather than by the image, so a very large
        // screenshot from a tool call does not push the dialog off the screen.
        <img src={url} alt={entry.name} className="file-preview__image" />
      )}

      {!image &&
        (notText ? (
          <p className="notice">
            <FileWarning />
            <span>{t('files.notText')}</span>
          </p>
        ) : (
          <FileEditor
            entry={entry}
            workspace={workspace}
            {...(onDirtyChange ? { onDirtyChange } : {})}
          />
        ))}

      <div className="cluster">
        <Button asChild variant="secondary">
          {/* `rel` on a `_blank` link: without it the opened tab gets a handle
              on this one through `window.opener`. */}
          <a
            href={url}
            target="_blank"
            rel="noreferrer noopener"
            download={entry.name}
          >
            <Download />
            Download
          </a>
        </Button>
        <span className="file-preview__meta">
          {formatBytes(entry.sizeBytes)} · {entry.mimeType ?? 'unknown type'}
        </span>
      </div>
    </div>
  );
}
