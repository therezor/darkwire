/**
 * What the user attached, under the message they attached it to.
 *
 * An attachment is a workspace file, so its identity is a path — which is what
 * makes this component possible at all. The previous shape put a signed URL in
 * the stored message; those expire in ten minutes, so a transcript reopened the
 * next morning held nothing but dead links, and the UI showed a text badge
 * rather than admit it.
 *
 * Two decisions:
 *
 *  - **An image renders as an image, and its URL is minted on render.**
 *    `<img src>` cannot carry an `Authorization` header, so the bytes come
 *    through `POST /api/files/signed-url` — an HMAC token naming one path, with
 *    a short life — exactly as the file browser's preview does. `gcTime: 0`
 *    because a URL that expired while the tab sat open is not a cached answer
 *    worth keeping; the next render asks again.
 *
 *  - **Everything else is a chip with a download link**, and the link is the
 *    point. A `.pdf` the model could only be told the path of is still a file
 *    the person who attached it may want back.
 *
 * `isImage` decides from the MIME type the *server* determined, never from the
 * filename: `/api/media/:token` refuses to inline anything its table does not
 * know, so guessing here would draw an `<img>` around a response the browser is
 * being told to download, and the reader would see a broken image.
 */

import { useQuery } from '@tanstack/react-query';
import { Download, File } from 'lucide-react';
import type { JSX } from 'react';
import { useTranslation } from 'react-i18next';

import type { Attachment } from '@ghostwire/protocol';

import { api } from '@/lib/api.js';
import { formatBytes } from '@/lib/format.js';
import { queryKeys } from '@/lib/query.js';
import { isImage } from '@/files/paths.js';

interface AttachmentListProps {
  readonly attachments: readonly Attachment[];
  /** Which workspace the paths are relative to. */
  readonly workspace: string;
}

/** The last segment of a workspace path, for an attachment that lost its name. */
function basename(path: string): string {
  return path.slice(path.lastIndexOf('/') + 1);
}

function AttachmentItem({
  attachment,
  workspace,
}: {
  readonly attachment: Attachment;
  readonly workspace: string;
}): JSX.Element {
  const { t } = useTranslation();
  const label = attachment.name ?? basename(attachment.path);

  const signed = useQuery({
    queryKey: queryKeys.fileUrl(workspace, attachment.path),
    queryFn: ({ signal }) => api.signUrl(workspace, attachment.path, signal),
    gcTime: 0,
  });

  // A thumbnail that appears a moment later is fine; a spinner per attachment
  // in a scrolling transcript is not. So the chip is what renders until the URL
  // is in hand, and an image simply becomes one when signing fails — a file
  // deleted out from under an old message is a chip, not a broken picture.
  if (isImage(attachment.mimeType) && signed.data !== undefined) {
    return (
      <li className="message-attachment message-attachment--image">
        <a href={signed.data.url} target="_blank" rel="noreferrer noopener">
          <img src={signed.data.url} alt={label} loading="lazy" />
        </a>
      </li>
    );
  }

  return (
    <li className="message-attachment">
      <File aria-hidden="true" className="message-attachment__icon" />
      <span className="message-attachment__name truncate">{label}</span>
      {attachment.sizeBytes !== undefined && (
        <span className="message-attachment__size">
          {formatBytes(attachment.sizeBytes)}
        </span>
      )}
      {signed.data !== undefined && (
        <a
          href={signed.data.url}
          download={label}
          rel="noreferrer noopener"
          aria-label={t('chat.attachmentDownload', { name: label })}
          className="message-attachment__download"
        >
          <Download aria-hidden="true" />
        </a>
      )}
    </li>
  );
}

export function AttachmentList({
  attachments,
  workspace,
}: AttachmentListProps): JSX.Element | null {
  if (attachments.length === 0) return null;
  return (
    <ul className="cluster message-user__attachments">
      {attachments.map((attachment, index) => (
        // The index is in the key because the path is not unique: attaching one
        // file twice to a message is legal, and two `<li>`s keyed alike is a
        // React warning and a lost re-render. Nothing reorders this list, so the
        // usual objection to an index key does not apply.
        <AttachmentItem
          key={`${String(index)}:${attachment.path}`}
          attachment={attachment}
          workspace={workspace}
        />
      ))}
    </ul>
  );
}
