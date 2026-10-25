/**
 * Tracked App Time — management screen (soft-fork feature).
 *
 * A self-contained top-level view: list tracked apps with today's merged total,
 * add an app (manual-only, or linked to an Android package for automatic
 * collection where that exists), rename/archive, log app time by hand, and drill into a
 * day's contributing entries (adjust or delete each). Automatic Android
 * collection fills the same data from the native side; this screen never needs
 * platform-specific code — it only reads and writes rows through the shared API.
 */
import { Component } from '../component';
import { html } from '../html';
import { Logger } from '../logger';
import { customPrompt, customConfirm, customAlert } from '../modal_base';
import { EVENTS } from '../constants';
import {
    isDesktop,
    getTrackedApps,
    addTrackedApp,
    updateTrackedApp,
    setTrackedAppArchived,
    addUsageEntry,
    setUsageEntryAdjustment,
    deleteUsageEntry,
    getUsageEntriesForDay,
    getTrackedAppDailyTotals,
    getTrackedAppTotalMinutes,
    refreshTrackedAppUsage,
    openUsageAccessSettings,
    exportTrackedAppUsageCsv,
    importTrackedAppUsageCsv,
} from '../api';
import type { TrackedApp, UsageEntry, AppDailyTotal, UsagePlatform } from '../types';

interface TrackedAppsState {
    date: string;
    apps: TrackedApp[];
    totals: AppDailyTotal[];
    totalMinutes: number;
    expandedUid: string | null;
    expandedEntries: UsageEntry[];
    loading: boolean;
    autoAvailable: boolean;
    permissionGranted: boolean;
    collecting: boolean;
}

export function todayLocalDate(): string {
    const now = new Date();
    const year = now.getFullYear();
    const month = String(now.getMonth() + 1).padStart(2, '0');
    const day = String(now.getDate()).padStart(2, '0');
    return `${year}-${month}-${day}`;
}

/** Render a minute count as a compact "Xh Ym" / "Ym" label. */
export function formatMinutes(minutes: number): string {
    const safe = Math.max(0, Math.round(minutes));
    const hours = Math.floor(safe / 60);
    const mins = safe % 60;
    if (hours === 0) return `${mins}m`;
    if (mins === 0) return `${hours}h`;
    return `${hours}h ${mins}m`;
}

/** Dotted Java-style identifier with at least two segments, e.g. com.ichi2.anki. */
const ANDROID_PACKAGE_PATTERN = /^[A-Za-z]\w*(\.[A-Za-z]\w*)+$/;

function manualPlatform(): UsagePlatform {
    return isDesktop() ? 'desktop' : 'web';
}

export class TrackedAppsView extends Component<TrackedAppsState> {
    constructor(container: HTMLElement) {
        super(container, {
            date: todayLocalDate(),
            apps: [],
            totals: [],
            totalMinutes: 0,
            expandedUid: null,
            expandedEntries: [],
            loading: true,
            autoAvailable: false,
            permissionGranted: true,
            collecting: false,
        });
    }

    /** Entry point used by the shell: show stored data, then collect in the
     *  background (Android) and refresh totals if anything changed. */
    async loadData(): Promise<void> {
        await this.loadDisplay();
        this.maybeCollect();
    }

    private async loadDisplay(): Promise<void> {
        try {
            const [apps, totals, totalMinutes] = await Promise.all([
                getTrackedApps(false),
                getTrackedAppDailyTotals(this.state.date),
                getTrackedAppTotalMinutes(this.state.date),
            ]);
            this.state = { ...this.state, apps, totals, totalMinutes, loading: false };
        } catch (error) {
            Logger.error('[tracked-apps] Failed to load tracked app data', error);
            this.state = { ...this.state, loading: false };
        }
        this.render();
    }

    /** Trigger an automatic-collection run (Android). A noop that reports
     *  `available: false` elsewhere, in which case no refresh UI is shown. */
    private async maybeCollect(): Promise<void> {
        if (this.state.collecting) return;
        this.state = { ...this.state, collecting: true };
        this.render();
        try {
            const outcome = await refreshTrackedAppUsage();
            this.state = {
                ...this.state,
                autoAvailable: outcome.available,
                permissionGranted: outcome.permissionGranted,
                collecting: false,
            };
            if (outcome.updatedEntries > 0) {
                await this.loadDisplay();
            } else {
                this.render();
            }
        } catch (error) {
            Logger.error('[tracked-apps] Automatic usage collection failed', error);
            this.state = { ...this.state, collecting: false };
            this.render();
        }
    }

    private totalFor(uid: string): number {
        return this.state.totals.find(total => total.tracked_app_uid === uid)?.total_minutes ?? 0;
    }

    private renderRefreshButton(): HTMLElement | null {
        if (!this.state.autoAvailable) return null;
        const label = this.state.collecting ? 'Refreshing…' : 'Refresh now';
        const disabledAttr = this.state.collecting ? 'disabled' : '';
        return html`<button class="btn btn-secondary" id="tracked-apps-refresh" ${disabledAttr}>${label}</button>`;
    }

    render(): void {
        const { apps, loading, date, totalMinutes, autoAvailable, permissionGranted } = this.state;

        const refreshButton = this.renderRefreshButton();
        const showBanner = autoAvailable && !permissionGranted;

        const root = html`
            <div class="tracked-apps-view">
                <header class="tracked-apps-header">
                    <div>
                        <h1 class="tracked-apps-title">Tracked Apps</h1>
                        <p class="tracked-apps-subtitle">Time in your study apps, merged across every device.</p>
                    </div>
                    <div class="tracked-apps-header-actions">
                        ${refreshButton}
                        <button class="btn btn-secondary" id="tracked-apps-export">Export CSV</button>
                        <button class="btn btn-secondary" id="tracked-apps-import">Import CSV</button>
                        <button class="btn btn-primary" id="tracked-apps-add">Add app</button>
                    </div>
                </header>

                ${
                    showBanner
                        ? html`
                            <div class="tracked-apps-banner" role="alert">
                                <span>Usage access was revoked for Kechimochi — tracked app data will stop updating until it's re-granted.</span>
                                <button class="btn btn-secondary btn-sm" id="tracked-apps-grant">Open settings</button>
                            </div>
                        `
                        : null
                }

                <section class="tracked-apps-total-card">
                    <span class="tracked-apps-total-label">App time on ${date}</span>
                    <span class="tracked-apps-total-value">${formatMinutes(totalMinutes)}</span>
                </section>

                <section class="tracked-apps-logform">
                    <div class="tracked-apps-logform-row">
                        <label class="tracked-apps-field">
                            <span>App</span>
                            <select id="tracked-apps-log-app"></select>
                        </label>
                        <label class="tracked-apps-field">
                            <span>Date</span>
                            <input type="date" id="tracked-apps-log-date" value="${date}" />
                        </label>
                        <label class="tracked-apps-field tracked-apps-field--minutes">
                            <span>Minutes</span>
                            <input type="number" id="tracked-apps-log-minutes" min="0" step="1" placeholder="30" />
                        </label>
                        <button class="btn btn-primary" id="tracked-apps-log-submit">Log time</button>
                    </div>
                </section>

                <section class="tracked-apps-list" id="tracked-apps-list"></section>
            </div>
        `;

        const listEl = root.querySelector('#tracked-apps-list') as HTMLElement;
        if (loading) {
            listEl.appendChild(html`<p class="tracked-apps-empty">Loading…</p>`);
        } else if (apps.length === 0) {
            listEl.appendChild(
                html`<p class="tracked-apps-empty">No tracked apps yet. Add one to start logging time.</p>`,
            );
        } else {
            for (const app of apps) {
                listEl.appendChild(this.renderAppRow(app));
            }
        }

        this.populateLogAppOptions(root);
        this.wireControls(root);

        this.clear();
        this.container.appendChild(root);
    }

    private renderAppRow(app: TrackedApp): HTMLElement {
        const uid = app.uid ?? '';
        const total = this.totalFor(uid);
        const expanded = this.state.expandedUid === uid;
        const badges: string[] = [];
        if (app.auto_tracking_enabled) badges.push('Auto');
        if (app.manual_entry_enabled) badges.push('Manual');

        const row = html`
            <div class="tracked-apps-row ${expanded ? 'is-expanded' : ''}">
                <div class="tracked-apps-row-main" role="button" tabindex="0" data-uid="${uid}">
                    <span class="tracked-apps-row-name">${app.display_name}</span>
                    <span class="tracked-apps-row-badges">${badges.join(' · ')}</span>
                    <span class="tracked-apps-row-total">${formatMinutes(total)}</span>
                </div>
                <div class="tracked-apps-row-actions">
                    <button class="btn btn-secondary btn-sm" data-action="rename" data-id="${app.id}">Rename</button>
                    ${
                        this.state.autoAvailable
                            ? html`<button class="btn btn-secondary btn-sm" data-action="package" data-id="${app.id}">Android ID</button>`
                            : null
                    }
                    <button class="btn btn-danger btn-sm" data-action="archive" data-id="${app.id}">Archive</button>
                </div>
            </div>
        `;

        if (expanded) {
            const drill = html`<div class="tracked-apps-drill" id="tracked-apps-drill"></div>`;
            const entries = this.state.expandedEntries;
            if (entries.length === 0) {
                drill.appendChild(
                    html`<p class="tracked-apps-drill-empty">No entries on ${this.state.date}.</p>`,
                );
            } else {
                for (const entry of entries) {
                    drill.appendChild(this.renderEntryRow(entry));
                }
            }
            row.appendChild(drill);
        }
        return row;
    }

    private renderEntryRow(entry: UsageEntry): HTMLElement {
        const displayed = entry.manual_adjustment_minutes ?? entry.minutes;
        const adjusted = entry.manual_adjustment_minutes != null;
        const rawNote = adjusted ? ` (raw ${formatMinutes(entry.minutes)})` : '';
        const sourceLabel = entry.source === 'android_auto' ? 'Automatic' : 'Manual';
        return html`
            <div class="tracked-apps-entry">
                <span class="tracked-apps-entry-meta">${sourceLabel} · ${entry.platform}</span>
                <span class="tracked-apps-entry-minutes">${formatMinutes(displayed)}${rawNote}</span>
                <span class="tracked-apps-entry-actions">
                    <button class="btn btn-secondary btn-sm" data-entry-action="adjust" data-id="${entry.id}">Adjust</button>
                    <button class="btn btn-danger btn-sm" data-entry-action="delete" data-id="${entry.id}">Delete</button>
                </span>
            </div>
        `;
    }

    private populateLogAppOptions(root: HTMLElement): void {
        const select = root.querySelector('#tracked-apps-log-app') as HTMLSelectElement | null;
        if (!select) return;
        const loggable = this.state.apps.filter(app => app.manual_entry_enabled && app.uid);
        if (loggable.length === 0) {
            const option = document.createElement('option');
            option.value = '';
            option.textContent = 'No apps yet';
            select.appendChild(option);
            select.disabled = true;
            return;
        }
        for (const app of loggable) {
            const option = document.createElement('option');
            option.value = app.uid as string;
            option.textContent = app.display_name;
            select.appendChild(option);
        }
    }

    private wireControls(root: HTMLElement): void {
        root.querySelector('#tracked-apps-add')?.addEventListener('click', () => this.handleAddApp());
        root.querySelector('#tracked-apps-export')?.addEventListener('click', () => this.handleExportCsv());
        root.querySelector('#tracked-apps-import')?.addEventListener('click', () => this.handleImportCsv());
        root.querySelector('#tracked-apps-refresh')?.addEventListener('click', () => this.maybeCollect());
        root.querySelector('#tracked-apps-grant')?.addEventListener('click', async () => {
            await openUsageAccessSettings();
            // Re-check permission when the user returns from settings.
            await this.maybeCollect();
        });

        const dateInput = root.querySelector('#tracked-apps-log-date') as HTMLInputElement | null;
        dateInput?.addEventListener('change', () => {
            const value = dateInput.value.trim();
            if (value) {
                this.state = { ...this.state, date: value, expandedUid: null, expandedEntries: [] };
                this.loadData();
            }
        });

        root.querySelector('#tracked-apps-log-submit')?.addEventListener('click', () => this.handleLogTime(root));

        root.querySelectorAll('.tracked-apps-row-main').forEach(el => {
            const toggle = () => this.toggleDrill((el as HTMLElement).dataset.uid ?? '');
            el.addEventListener('click', toggle);
            el.addEventListener('keydown', event => {
                const key = (event as KeyboardEvent).key;
                if (key === 'Enter' || key === ' ') {
                    event.preventDefault();
                    toggle();
                }
            });
        });

        root.querySelectorAll('[data-action]').forEach(el => {
            el.addEventListener('click', event => {
                event.stopPropagation();
                const button = el as HTMLElement;
                const id = Number(button.dataset.id);
                if (button.dataset.action === 'rename') this.handleRename(id);
                else if (button.dataset.action === 'package') this.handleSetPackage(id);
                else if (button.dataset.action === 'archive') this.handleArchive(id);
            });
        });

        root.querySelectorAll('[data-entry-action]').forEach(el => {
            el.addEventListener('click', event => {
                event.stopPropagation();
                const button = el as HTMLElement;
                const id = Number(button.dataset.id);
                if (button.dataset.entryAction === 'adjust') this.handleAdjust(id);
                else if (button.dataset.entryAction === 'delete') this.handleDeleteEntry(id);
            });
        });
    }

    private notifyDataChanged(): void {
        globalThis.dispatchEvent(new CustomEvent(EVENTS.LOCAL_DATA_CHANGED, { detail: {} }));
    }

    private async handleExportCsv(): Promise<void> {
        try {
            const count = await exportTrackedAppUsageCsv();
            if (count > 0) {
                await customAlert('Exported', `Exported ${count} usage ${count === 1 ? 'entry' : 'entries'}.`);
            }
        } catch (error) {
            await customAlert('Could not export usage CSV', String(error));
        }
    }

    private async handleImportCsv(): Promise<void> {
        try {
            const count = await importTrackedAppUsageCsv();
            if (count > 0) {
                this.notifyDataChanged();
                await this.loadDisplay();
                await customAlert('Imported', `Imported ${count} usage ${count === 1 ? 'entry' : 'entries'}.`);
            }
        } catch (error) {
            await customAlert('Could not import usage CSV', String(error));
        }
    }

    private async handleAddApp(): Promise<void> {
        const name = await customPrompt('Add tracked app', '', 'Name this app (e.g. Windows Anki).');
        if (name === null) return;
        const trimmed = name.trim();
        if (!trimmed) return;
        // Where automatic collection exists (Android), optionally link the app
        // to its package so the collector picks it up; blank = manual-only.
        let packageName: string | null = null;
        if (this.state.autoAvailable) {
            const picked = await this.promptPackageName('');
            if (picked === undefined) return;
            packageName = picked;
        }
        try {
            await addTrackedApp({
                display_name: trimmed,
                android_package_name: packageName,
                auto_tracking_enabled: packageName !== null,
                manual_entry_enabled: true,
                archived: false,
            });
            this.notifyDataChanged();
            await this.loadData();
        } catch (error) {
            await customAlert('Could not add app', String(error));
        }
    }

    /** Link (or unlink) an existing app to an Android package for automatic collection. */
    private async handleSetPackage(id: number): Promise<void> {
        const app = this.state.apps.find(candidate => candidate.id === id);
        if (!app) return;
        const current = app.android_package_name ?? '';
        const picked = await this.promptPackageName(current, id);
        if (picked === undefined || (picked ?? '') === current) return;
        try {
            await updateTrackedApp({
                ...app,
                android_package_name: picked,
                auto_tracking_enabled: picked !== null,
            });
            this.notifyDataChanged();
            await this.loadData();
        } catch (error) {
            await customAlert('Could not update app', String(error));
        }
    }

    /** Ask for an Android package ID. Resolves to the validated ID, `null` for
     *  blank (manual-only), or `undefined` when cancelled or rejected. */
    private async promptPackageName(current: string, excludeId?: number): Promise<string | null | undefined> {
        const input = await customPrompt(
            'Android package ID',
            current,
            'Optional: the package ID to track automatically, e.g. com.ichi2.anki for AnkiDroid. ' +
                'It is the "id=" part of the app\'s Play Store link. Leave blank for manual-only.',
        );
        if (input === null) return undefined;
        const trimmed = input.trim();
        if (!trimmed) return null;
        if (!ANDROID_PACKAGE_PATTERN.test(trimmed)) {
            await customAlert('Invalid package ID', `"${trimmed}" is not a valid Android package ID (e.g. com.ichi2.anki).`);
            return undefined;
        }
        const clash = this.state.apps.find(
            candidate => candidate.id !== excludeId && candidate.android_package_name === trimmed,
        );
        if (clash) {
            await customAlert('Package already tracked', `"${trimmed}" is already linked to "${clash.display_name}".`);
            return undefined;
        }
        return trimmed;
    }

    private async handleRename(id: number): Promise<void> {
        const app = this.state.apps.find(candidate => candidate.id === id);
        if (!app) return;
        const name = await customPrompt('Rename tracked app', app.display_name);
        if (name === null) return;
        const trimmed = name.trim();
        if (!trimmed || trimmed === app.display_name) return;
        try {
            await updateTrackedApp({ ...app, display_name: trimmed });
            this.notifyDataChanged();
            await this.loadData();
        } catch (error) {
            await customAlert('Could not rename app', String(error));
        }
    }

    private async handleArchive(id: number): Promise<void> {
        const app = this.state.apps.find(candidate => candidate.id === id);
        if (!app) return;
        const confirmed = await customConfirm(
            'Archive tracked app',
            `Hide "${app.display_name}" from the list? Its logged time is kept.`,
            'btn-danger',
            'Archive',
        );
        if (!confirmed) return;
        try {
            await setTrackedAppArchived(id, true);
            this.notifyDataChanged();
            await this.loadData();
        } catch (error) {
            await customAlert('Could not archive app', String(error));
        }
    }

    private async handleLogTime(root: HTMLElement): Promise<void> {
        const appSelect = root.querySelector('#tracked-apps-log-app') as HTMLSelectElement | null;
        const dateInput = root.querySelector('#tracked-apps-log-date') as HTMLInputElement | null;
        const minutesInput = root.querySelector('#tracked-apps-log-minutes') as HTMLInputElement | null;
        const trackedAppUid = appSelect?.value ?? '';
        const date = dateInput?.value?.trim() ?? '';
        const minutes = Number(minutesInput?.value);

        if (!trackedAppUid) {
            await customAlert('Pick an app', 'Add a tracked app before logging time.');
            return;
        }
        if (!date) {
            await customAlert('Pick a date', 'Choose the day this time was spent.');
            return;
        }
        if (!Number.isFinite(minutes) || minutes <= 0) {
            await customAlert('Enter minutes', 'Enter how many minutes to log (a positive number).');
            return;
        }

        try {
            await addUsageEntry({
                tracked_app_uid: trackedAppUid,
                date,
                minutes: Math.round(minutes),
                source: 'manual',
                platform: manualPlatform(),
            });
            if (minutesInput) minutesInput.value = '';
            this.notifyDataChanged();
            // Logging onto the shown day keeps the drill-down in sync.
            if (date === this.state.date && this.state.expandedUid) {
                await this.refreshExpandedEntries();
            }
            await this.loadData();
        } catch (error) {
            await customAlert('Could not log time', String(error));
        }
    }

    private async toggleDrill(uid: string): Promise<void> {
        if (!uid) return;
        if (this.state.expandedUid === uid) {
            this.state = { ...this.state, expandedUid: null, expandedEntries: [] };
            this.render();
            return;
        }
        this.state = { ...this.state, expandedUid: uid, expandedEntries: [] };
        this.render();
        await this.refreshExpandedEntries();
    }

    private async refreshExpandedEntries(): Promise<void> {
        const uid = this.state.expandedUid;
        if (!uid) return;
        try {
            const entries = await getUsageEntriesForDay(uid, this.state.date);
            if (this.state.expandedUid === uid) {
                this.state = { ...this.state, expandedEntries: entries };
                this.render();
            }
        } catch (error) {
            Logger.error('[tracked-apps] Failed to load drill-down entries', error);
        }
    }

    private async handleAdjust(id: number): Promise<void> {
        const entry = this.state.expandedEntries.find(candidate => candidate.id === id);
        if (!entry) return;
        const current = entry.manual_adjustment_minutes ?? entry.minutes;
        const input = await customPrompt(
            'Adjust entry',
            String(current),
            `Corrected minutes for this entry (raw value ${formatMinutes(entry.minutes)} is kept).`,
        );
        if (input === null) return;
        const trimmed = input.trim();
        const value = trimmed === '' ? null : Number(trimmed);
        if (value !== null && (!Number.isFinite(value) || value < 0)) {
            await customAlert('Invalid value', 'Enter a non-negative number of minutes, or leave blank to clear.');
            return;
        }
        try {
            await setUsageEntryAdjustment(id, value === null ? null : Math.round(value));
            this.notifyDataChanged();
            await this.refreshExpandedEntries();
            await this.loadData();
        } catch (error) {
            await customAlert('Could not adjust entry', String(error));
        }
    }

    private async handleDeleteEntry(id: number): Promise<void> {
        const confirmed = await customConfirm(
            'Delete entry',
            'Remove this usage entry? This cannot be undone.',
            'btn-danger',
            'Delete',
        );
        if (!confirmed) return;
        try {
            await deleteUsageEntry(id);
            this.notifyDataChanged();
            await this.refreshExpandedEntries();
            await this.loadData();
        } catch (error) {
            await customAlert('Could not delete entry', String(error));
        }
    }
}
