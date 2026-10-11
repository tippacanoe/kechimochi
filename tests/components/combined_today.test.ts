import { describe, it, expect, vi, beforeEach } from 'vitest';
import { CombinedTodayCard } from '../../src/tracked_apps/CombinedTodayCard';
import { todayLocalDate } from '../../src/tracked_apps/TrackedAppsView';
import * as api from '../../src/api';

vi.mock('../../src/api', () => ({
    getHeatmap: vi.fn(),
    getTrackedAppTotalMinutes: vi.fn(),
}));

const mockedApi = vi.mocked(api);

function mountCard(): { card: CombinedTodayCard; container: HTMLElement } {
    const container = document.createElement('div');
    document.body.appendChild(container);
    return { card: new CombinedTodayCard(container), container };
}

describe('CombinedTodayCard', () => {
    beforeEach(() => {
        vi.clearAllMocks();
    });

    it('sums immersion and tracked-app minutes for today with a breakdown', async () => {
        const today = todayLocalDate();
        mockedApi.getHeatmap.mockResolvedValue([
            { date: today, total_minutes: 50, total_characters: 0 },
            { date: '2000-01-01', total_minutes: 999, total_characters: 0 },
        ]);
        mockedApi.getTrackedAppTotalMinutes.mockResolvedValue(70);

        const { card, container } = mountCard();
        await card.refresh();

        expect(container.querySelector('.combined-today-value')?.textContent).toBe('2h');
        const breakdown = container.querySelector('.combined-today-breakdown')?.textContent ?? '';
        expect(breakdown).toContain('Immersion 50m');
        expect(breakdown).toContain('Apps 1h 10m');
        expect(mockedApi.getTrackedAppTotalMinutes).toHaveBeenCalledWith(today);
    });

    it('treats a day with no immersion entry as zero immersion', async () => {
        mockedApi.getHeatmap.mockResolvedValue([]);
        mockedApi.getTrackedAppTotalMinutes.mockResolvedValue(25);

        const { card, container } = mountCard();
        await card.refresh();

        expect(container.querySelector('.combined-today-value')?.textContent).toBe('25m');
    });
});
