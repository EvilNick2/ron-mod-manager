<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, onMounted, ref, watch } from "vue";
import { store } from "../store";

const current = computed(() => store.dialogs[0] ?? null);
const value = ref("");
const error = ref("");
const input = ref<HTMLInputElement | null>(null);
const primary = ref<HTMLButtonElement | null>(null);
const cancel = ref<HTMLButtonElement | null>(null);
const card = ref<HTMLElement | null>(null);
let restoreFocus: HTMLElement | null = null;

const hasInput = computed(() => current.value?.kind === "prompt" || current.value?.kind === "copy");
const hasCancel = computed(() => ["confirm", "prompt", "choice"].includes(current.value?.kind ?? ""));
const isChoice = computed(() => current.value?.kind === "choice");

watch(() => current.value?.id, async (_id, previous) => {
  const d = current.value;
  if (!d) {
    const target = restoreFocus;
    restoreFocus = null;
    if (target?.isConnected) target.focus();
    return;
  }
  if (previous === undefined) {
    restoreFocus = document.activeElement instanceof HTMLElement ? document.activeElement : null;
  }
  value.value = d.value;
  error.value = "";
  await nextTick();
  if (hasInput.value && input.value) {
    input.value.focus();
    input.value.select();
  } else if (d.kind === "choice") {
    card.value?.querySelector<HTMLButtonElement>(".choice-btn")?.focus();
  } else if (d.danger && cancel.value) {
    cancel.value.focus();
  } else {
    primary.value?.focus();
  }
}, { immediate: true });

function settle(result: boolean | string | null) {
  const d = store.dialogs.shift();
  d?.resolve(result);
}

function confirmDialog() {
  const d = current.value;
  if (!d || d.kind === "choice") return;
  if (d.kind === "prompt") {
    const message = d.validate?.(value.value) ?? null;
    if (message) {
      error.value = message;
      input.value?.focus();
      return;
    }
    settle(value.value);
  } else {
    settle(d.kind === "confirm" ? true : null);
  }
}

function cancelDialog() {
  const d = current.value;
  if (!d) return;
  settle(d.kind === "confirm" ? false : null);
}

function onInput() {
  if (error.value) error.value = current.value?.validate?.(value.value) ?? "";
}

// Capture phase, so modals underneath never see the keys a dialog handles
function onKeydown(e: KeyboardEvent) {
  if (!current.value) return;
  const inCard = e.target instanceof Node && !!card.value?.contains(e.target);
  if (e.key === "Enter") {
    e.preventDefault();
    e.stopPropagation();
    if (inCard && e.target instanceof HTMLButtonElement) e.target.click();
    else confirmDialog();
  } else if (e.key === "Escape") {
    e.preventDefault();
    e.stopPropagation();
    cancelDialog();
  } else if (e.key === "Tab") {
    trapTab(e, inCard);
  }
}

function trapTab(e: KeyboardEvent, inCard: boolean) {
  const items = Array.from(card.value?.querySelectorAll<HTMLElement>("input, button") ?? []);
  if (items.length === 0) return;
  const first = items[0];
  const last = items[items.length - 1];
  if (!inCard || (e.shiftKey && e.target === first) || (!e.shiftKey && e.target === last)) {
    e.preventDefault();
    e.stopPropagation();
    (e.shiftKey ? last : first).focus();
  }
}

onMounted(() => window.addEventListener("keydown", onKeydown, { capture: true }));
onBeforeUnmount(() => window.removeEventListener("keydown", onKeydown, { capture: true }));
</script>

<template>
  <transition name="fade">
    <div v-if="current" class="modal-overlay">
      <div ref="card" class="dialog-modal" :class="{ danger: current.danger }" role="dialog" aria-modal="true">
        <h2 v-if="current.title" class="modal-title">{{ current.title }}</h2>
        <p v-if="current.message" class="dialog-message">{{ current.message }}</p>
        <input
          v-if="hasInput"
          ref="input"
          v-model="value"
          class="dialog-input"
          :class="{ invalid: !!error }"
          :readonly="current.kind === 'copy'"
          spellcheck="false"
          @input="onInput"
        />
        <p v-if="error" class="dialog-error">{{ error }}</p>
        <div v-if="isChoice" class="choice-list">
          <button
            v-for="choice in current.choices ?? []"
            :key="choice.value"
            class="choice-btn"
            @click="settle(choice.value)"
          >{{ choice.label }}</button>
        </div>
        <div class="modal-actions">
          <button v-if="hasCancel" ref="cancel" class="secondary-btn" @click="cancelDialog">{{ current.cancelLabel }}</button>
          <button v-if="!isChoice" ref="primary" class="primary-btn" @click="confirmDialog">{{ current.confirmLabel }}</button>
        </div>
      </div>
    </div>
  </transition>
</template>

<style scoped>
.modal-overlay {
  position: fixed;
  top: 0; left: 0; right: 0; bottom: 0;
  background: rgba(0, 0, 0, 0.6);
  backdrop-filter: blur(8px);
  z-index: 4000;
  display: flex;
  justify-content: center;
  align-items: center;
}

.dialog-modal {
  background: var(--bg-dark);
  border: 1px solid var(--border-light);
  border-radius: 12px;
  width: 500px;
  max-width: 90vw;
  max-height: 84vh;
  overflow: auto;
  padding: 2.5rem;
  box-shadow: 0 25px 50px -12px rgba(0, 0, 0, 1);
  display: flex;
  flex-direction: column;
  user-select: text;
}

.dialog-modal.danger {
  border-color: var(--accent-primary);
}

.modal-title {
  margin: 0 0 1.5rem 0;
  color: #fff;
  font-size: 1.5rem;
  font-weight: 700;
  text-align: center;
}

.dialog-modal.danger .modal-title {
  color: var(--accent-primary);
}

.dialog-message {
  color: var(--text-muted);
  font-size: 0.95rem;
  line-height: 1.5;
  margin: 0 0 1.5rem;
  white-space: pre-line;
  overflow-wrap: anywhere;
}

.dialog-input {
  background: var(--bg-card);
  border: 1px solid var(--border-light);
  border-radius: 8px;
  color: var(--text-main);
  font-size: 0.95rem;
  padding: 0.75rem 1rem;
  margin: 0 0 1rem;
  outline: none;
  transition: border-color 0.2s;
}

.dialog-input:focus {
  border-color: var(--accent-primary);
}

.dialog-input.invalid {
  border-color: var(--accent-hover);
}

.dialog-error {
  color: var(--accent-hover);
  font-size: 0.85rem;
  line-height: 1.4;
  margin: -0.5rem 0 1rem;
}

.choice-list {
  display: flex;
  flex-direction: column;
  gap: 0.5rem;
  margin: 0 0 1rem;
}

.choice-btn {
  background: var(--bg-card);
  border: 1px solid var(--border-light);
  border-radius: 8px;
  color: var(--text-main);
  font-size: 0.95rem;
  font-weight: 600;
  padding: 0.75rem 1rem;
  text-align: left;
  cursor: pointer;
  transition: all 0.2s;
}

.choice-btn:hover {
  background: var(--bg-hover);
  border-color: var(--text-muted);
}

.choice-btn:focus-visible {
  outline: 2px solid var(--text-main);
  outline-offset: 2px;
}

.modal-actions {
  display: flex;
  justify-content: flex-end;
  gap: 1rem;
  margin-top: 1rem;
}

.secondary-btn, .primary-btn {
  padding: 0.75rem 1.75rem;
  border-radius: 8px;
  font-weight: 700;
  font-size: 0.95rem;
  cursor: pointer;
  transition: all 0.2s;
  border: none;
}

.secondary-btn {
  background: transparent;
  color: var(--text-muted);
}

.secondary-btn:hover {
  color: #fff;
  background: var(--bg-card);
}

.primary-btn {
  background: var(--accent-primary);
  color: var(--text-main);
  box-shadow: 0 4px 6px -1px rgba(230,29,54,0.3);
}

.primary-btn:hover {
  background: var(--accent-hover);
  transform: translateY(-2px);
}

.primary-btn:focus-visible, .secondary-btn:focus-visible {
  outline: 2px solid var(--text-main);
  outline-offset: 2px;
}
</style>
