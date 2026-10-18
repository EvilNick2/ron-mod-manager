<script setup lang="ts">
import { computed } from "vue";
import { store } from "../store";
import { hasVisibleOps, progressLine, progressPercent } from "../packActions";

const ops = computed(() => Object.keys(store.inFlight)
  .filter((id) => !store.progressHidden[id])
  .map((id) => {
    const p = store.packProgress[id];
    return {
      key: id,
      name: store.packs.find((pack) => pack.pack_id === id)?.name ?? store.opNames[id] ?? id,
      line: p ? progressLine(p) : "Starting...",
      percent: p ? progressPercent(p) : 0,
    };
  }));

function hide() {
  for (const id of Object.keys(store.inFlight)) store.progressHidden[id] = true;
}
</script>

<template>
  <transition name="fade">
    <div v-if="hasVisibleOps()" class="modal-overlay">
      <div class="progress-modal">
        <h2 class="modal-title">{{ ops.length > 1 ? "Modpacks In Progress" : "Modpack In Progress" }}</h2>
        <div v-for="op in ops" :key="op.key" class="op">
          <p class="op-name">{{ op.name }}</p>
          <p class="op-line">{{ op.line }}</p>
          <div class="op-track"><div class="op-fill" :style="{ width: op.percent + '%' }"></div></div>
        </div>
        <div class="modal-actions">
          <button class="secondary-btn" @click="hide">Hide</button>
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
  z-index: 1900;
  display: flex;
  justify-content: center;
  align-items: center;
}

.progress-modal {
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
}

.modal-title {
  margin: 0 0 1.5rem 0;
  color: #fff;
  font-size: 1.5rem;
  font-weight: 700;
  text-align: center;
}

.op {
  margin: 0 0 1.25rem;
}

.op-name {
  color: var(--text-main);
  font-weight: 700;
  font-size: 0.95rem;
  margin: 0 0 0.25rem;
}

.op-line {
  color: var(--text-muted);
  font-size: 0.85rem;
  margin: 0 0 0.5rem;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.op-track {
  height: 6px;
  border-radius: 3px;
  background: var(--bg-card);
  overflow: hidden;
}

.op-fill {
  height: 100%;
  background: var(--accent-primary);
  transition: width 0.2s;
}

.modal-actions {
  display: flex;
  justify-content: flex-end;
  gap: 1rem;
  margin-top: 0.5rem;
}

.secondary-btn {
  padding: 0.75rem 1.75rem;
  border-radius: 8px;
  font-weight: 700;
  font-size: 0.95rem;
  cursor: pointer;
  transition: all 0.2s;
  border: none;
  background: transparent;
  color: var(--text-muted);
}

.secondary-btn:hover {
  color: #fff;
  background: var(--bg-card);
}
</style>
