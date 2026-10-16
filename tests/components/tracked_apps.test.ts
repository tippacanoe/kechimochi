import { describe, it, expect, vi, beforeEach } from 'vitest';
import { TrackedAppsView, formatMinutes } from '../../src/tracked_apps/TrackedAppsView';
import * as api from '../../src/api';
import type { TrackedApp, AppDailyTotal } from '../../src/types';

vi.mock('../../src/api', () => ({
    isDesktop: vi.fn(() => true),
    getTrackedApps: vi.fn(),
    addTrackedApp: vi.fn(() => Promise.resolve(1)),
    updateTrackedApp: vi.fn(() => Promise.resolve()),
    setTrackedAppArchived: vi.fn(() => Promise.resolve()),
    addUsageEntry: vi.fn(() => Promise.resolve(1)),
    setUsageEntryAdjustment: vi.fn(() => Promise.resolve()),
    deleteUsageEntry: vi.fn(() => Promise.resolve()),
    getUsageEntriesForDay: vi.fn(() => Promise.resolve([])),
    getTrackedAppDailyTotals: vi.fn(),
    getTrackedAppTotalMinutes: vi.fn(),
    refreshTrackedAppUsage: vi.fn(() =>
        Promise.resolve({ available: false, permissionGranted: true, updatedEntries: 0 })),
    openUsageAccessSettings: vi.fn(() => Promise.resolve()),
    exportTrackedAppUsageCsv: vi.fn(() => Promise.resolve(0)),
    importTrackedAppUsageCsv: vi.fn(() => Promise.resolve(0)),
}));

vi.mock('../../src/modal_base', () => ({
    customPrompt: vi.fn(),
    customConfirm: vi.fn(() => Promise.resolve(true)),
    customAlert: vi.fn(() => Promise.resolve()),
}));

const mockedApi = vi.mocked(api);

function mountView(): { view: TrackedAppsView; container: HTMLElement } {
    const container = document.createElement('div');
    document.body.appendChild(container);
    const view = new TrackedAppsView(container);
    return { view, container };
}

const sampleApps: TrackedApp[] = [
    {
        id: 1,
        uid: 'app-anki',
        display_name: 'AnkiDroid',
        auto_tracking_enabled: true,
        manual_entry_enabled: true,
        archived: false,
    },
    {
        id: 2,
        uid: 'app-renshuu',
        display_name: 'Renshuu',
        auto_tracking_enabled: false,
        manual_entry_enabled: true,
        archived: false,
    },
];

const sampleTotals: AppDailyTotal[] = [
    { tracked_app_uid: 'app-anki', display_name: 'AnkiDroid', date: '2026-10-11', total_minutes: 75 },
    { tracked_app_uid: 'app-renshuu', display_name: 'Renshuu', date: '2026-10-11', total_minutes: 0 },
];

describe('formatMinutes', () => {
    it('renders compact hour/minute labels', () => {
        expect(formatMinutes(0)).toBe('0m');
        expect(formatMinutes(45)).toBe('45m');
        expect(formatMinutes(60)).toBe('1h');
        expect(formatMinutes(135)).toBe('2h 15m');
    });
});

describe('TrackedAppsView', () => {
    beforeEach(() => {
        vi.clearAllMocks();
        mockedApi.getTrackedApps.mockResolvedValue(sampleApps);
        mockedApi.getTrackedAppDailyTotals.mockResolvedValue(sampleTotals);
        mockedApi.getTrackedAppTotalMinutes.mockResolvedValue(75);
    });

    it('lists tracked apps with their merged daily totals', async () => {
        const { view, container } = mountView();
        await view.loadData();

        const names = Array.from(container.querySelectorAll('.tracked-apps-row-name')).map(
            el => el.textContent,
        );
        expect(names).toEqual(['AnkiDroid', 'Renshuu']);

        const totals = Array.from(container.querySelectorAll('.tracked-apps-row-total')).map(
            el => el.textContent,
        );
        expect(totals).toEqual(['1h 15m', '0m']);

        expect(container.querySelector('.tracked-apps-total-value')?.textContent).toBe('1h 15m');
    });

    it('logs manual time through the API with the chosen app, date and minutes', async () => {
        const { view, container } = mountView();
        await view.loadData();

        (container.querySelector('#tracked-apps-log-app') as HTMLSelectElement).value = 'app-renshuu';
        (container.querySelector('#tracked-apps-log-date') as HTMLInputElement).value = '2026-10-11';
        (container.querySelector('#tracked-apps-log-minutes') as HTMLInputElement).value = '25';

        (container.querySelector('#tracked-apps-log-submit') as HTMLButtonElement).click();
        await vi.waitFor(() => expect(mockedApi.addUsageEntry).toHaveBeenCalledTimes(1));

        expect(mockedApi.addUsageEntry).toHaveBeenCalledWith({
            tracked_app_uid: 'app-renshuu',
            date: '2026-10-11',
            minutes: 25,
            source: 'manual',
            platform: 'desktop',
        });
    });

    it('shows an empty state when there are no tracked apps', async () => {
        mockedApi.getTrackedApps.mockResolvedValue([]);
        mockedApi.getTrackedAppDailyTotals.mockResolvedValue([]);
        mockedApi.getTrackedAppTotalMinutes.mockResolvedValue(0);

        const { view, container } = mountView();
        await view.loadData();

        expect(container.querySelector('.tracked-apps-empty')?.textContent).toContain('No tracked apps yet');
    });

    it('hides the refresh affordance when automatic collection is unavailable', async () => {
        const { view, container } = mountView();
        await view.loadData();
        // refreshTrackedAppUsage defaults to available:false in this suite.
        await vi.waitFor(() =>
            expect(mockedApi.refreshTrackedAppUsage).toHaveBeenCalled(),
        );
        expect(container.querySelector('#tracked-apps-refresh')).toBeNull();
        expect(container.querySelector('.tracked-apps-banner')).toBeNull();
    });

    it('shows a revoked-permission banner when usage access is unavailable', async () => {
        mockedApi.refreshTrackedAppUsage.mockResolvedValue({
            available: true,
            permissionGranted: false,
            updatedEntries: 0,
        });

        const { view, container } = mountView();
        await view.loadData();

        await vi.waitFor(() => {
            const banner = container.querySelector('.tracked-apps-banner');
            expect(banner?.textContent).toContain('Usage access was revoked');
        });
        expect(container.querySelector('#tracked-apps-refresh')).not.toBeNull();
        container.querySelector<HTMLButtonElement>('#tracked-apps-grant')?.click();
        await vi.waitFor(() =>
            expect(mockedApi.openUsageAccessSettings).toHaveBeenCalled(),
        );
    });

    it('reloads totals after a collection run writes new entries', async () => {
        mockedApi.getTrackedAppTotalMinutes.mockResolvedValueOnce(0).mockResolvedValue(60);
        mockedApi.refreshTrackedAppUsage.mockResolvedValue({
            available: true,
            permissionGranted: true,
            updatedEntries: 3,
        });

        const { view, container } = mountView();
        await view.loadData();

        await vi.waitFor(() =>
            expect(container.querySelector('.tracked-apps-total-value')?.textContent).toBe('1h'),
        );
    });
});
