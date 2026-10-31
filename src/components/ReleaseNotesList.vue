<script setup lang="ts">
import { invoke } from "@tauri-apps/api/core";
import { ExternalLink } from "lucide-vue-next";
import { marked } from "marked";
import DOMPurify from "dompurify";

export type ReleaseNotesEntry = {
  version: string;
  // Shown as is, so callers pass a local date string
  publishedAt: string;
  // Markdown
  notes: string;
  url?: string;
  latest?: boolean;
};

defineProps<{ entries: ReleaseNotesEntry[] }>();

// Pack notes come from other players, so only the tags Markdown produces survive
const ALLOWED_TAGS = [
  "p", "br", "hr", "h1", "h2", "h3", "h4", "h5", "h6", "ul", "ol", "li", "strong", "em", "del",
  "code", "pre", "blockquote", "a", "table", "thead", "tbody", "tr", "th", "td",
];

function renderMarkdown(notes: string): string {
  const html = marked.parse(notes, { async: false, gfm: true, breaks: true });
  return DOMPurify.sanitize(html, { ALLOWED_TAGS, ALLOWED_ATTR: ["href", "title"] });
}

function webUrl(href: string): string | null {
  try {
    const url = new URL(href);
    return url.protocol === "http:" || url.protocol === "https:" ? url.href : null;
  } catch {
    return null;
  }
}

// Links in the notes open in the system browser, not inside the app window
function onNotesClick(event: MouseEvent) {
  const link = (event.target as HTMLElement).closest("a");
  if (!link) return;
  event.preventDefault();
  const url = webUrl(link.href);
  if (url) void openExternalUrl(url);
}

async function openExternalUrl(url: string) {
  if (!url) return;

  try {
    await invoke("open_browser_url", { url });
  } catch (e) {
    console.error("Failed to open URL:", e);
  }
}
</script>

<template>
  <ul class="release-history-list">
    <li class="release-history-item" v-for="release in entries" :key="release.version + release.publishedAt">
      <div class="release-history-item__header">
        <div class="release-history-item__version-row">
          <span class="release-history-item__version">v{{ release.version }}</span>
          <span class="status-pill status-ready" v-if="release.latest">Latest</span>
        </div>
        <span class="release-history-item__date">{{ release.publishedAt }}</span>
      </div>

      <div class="release-history-item__notes" v-html="renderMarkdown(release.notes)" @click="onNotesClick"></div>

      <button v-if="release.url" class="release-link" @click="openExternalUrl(release.url)">
        <ExternalLink :size="14" />
        Open release page
      </button>
    </li>
  </ul>
</template>

<style scoped>
.release-history-list {
  list-style: none;
  margin: 0;
  padding: 0;
  display: flex;
  flex-direction: column;
  gap: 8px;
  max-height: 360px;
  overflow-y: auto;
}

.release-history-item {
  border: 1px solid var(--border, var(--border-light));
  border-radius: 6px;
  background: var(--surface, var(--bg-dark));
  padding: 10px;
  display: flex;
  flex-direction: column;
  gap: 8px;
}

.release-history-item__header {
  display: flex;
  justify-content: space-between;
  gap: 8px;
  align-items: baseline;
}

.release-history-item__version-row {
  display: inline-flex;
  gap: 8px;
  align-items: center;
}

.release-history-item__version {
  font-size: 1.25rem;
  font-weight: 700;
}

.release-history-item__date {
  color: var(--muted, var(--text-muted));
  font-size: 12px;
}

.release-history-item__notes {
  font-size: 13px;
  line-height: 1.5;
}

.release-history-item__notes :deep(h2),
.release-history-item__notes :deep(h3) {
  margin: 0.75rem 0 0.25rem;
  font-size: 13px;
  font-weight: 700;
  color: var(--text-main);
}

.release-history-item__notes :deep(h2:first-child),
.release-history-item__notes :deep(h3:first-child) {
  margin-top: 0;
}

.release-history-item__notes :deep(ul) {
  margin: 0;
  padding-left: 1.25rem;
}

.release-history-item__notes :deep(p) {
  margin: 0 0 0.5rem;
}

.release-history-item__notes :deep(a) {
  color: var(--accent-primary);
}

.release-history-item__notes :deep(code) {
  font-size: 12px;
  background: var(--bg-card);
  padding: 0 4px;
  border-radius: 4px;
}

.status-pill {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  padding: 4px 8px;
  border-radius: 999px;
  font-size: 12px;
  font-weight: 600;
  border: 1px solid var(--border, var(--border-light));
  background: var(--secondary-surface, var(--bg-card));
}

.status-ready {
  color: var(--success-text, #1f6b3f);
  background: var(--success-bg, rgba(21, 128, 61, 0.12));
  border-color: var(--success-border, rgba(21, 128, 61, 0.2));
}

.release-link {
  border: 1px solid var(--border, var(--border-light));
  background: var(--button-bg, var(--bg-card));
  color: var(--button-text, var(--text-main));
  border-radius: 6px;
  padding: 7px 9px;
  display: inline-flex;
  gap: 6px;
  align-items: center;
  justify-content: center;
  font-weight: 600;
  cursor: pointer;
}

.release-link:hover:enabled {
  background: var(--button-hover, var(--bg-hover));
}
</style>
