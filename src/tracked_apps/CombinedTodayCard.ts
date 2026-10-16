/**
 * Combined "Japanese time today" metric (soft-fork feature, spec §5).
 *
 * Shows today's total study time as the sum of existing immersion-activity time
 * and tracked-app time, with the two contributing halves broken out underneath.
 * Self-contained: it reads immersion minutes from the activity heatmap and app
 * minutes from the tracked-app totals, so it can be dropped onto the dashboard
 * with a single mount and refreshed alongside the rest of the dashboard data.
 */
import { Component } from '../component';
import { html } from '../html';
import { Logger } from '../logger';
import { getHeatmap, getTrackedAppTotalMinutes } from '../api';
import { formatMinutes, todayLocalDate } from './TrackedAppsView';

interface CombinedTodayState {
    immersionMinutes: number;
    appMinutes: number;
    loaded: boolean;
}

export class CombinedTodayCard extends Component<CombinedTodayState> {
    constructor(container: HTMLElement) {
        super(container, { immersionMinutes: 0, appMinutes: 0, loaded: false });
    }

    async refresh(): Promise<void> {
        try {
            const today = todayLocalDate();
            const [heatmap, appMinutes] = await Promise.all([
                getHeatmap(),
                getTrackedAppTotalMinutes(today),
            ]);
            const immersionMinutes = heatmap.find(day => day.date === today)?.total_minutes ?? 0;
            this.state = { immersionMinutes, appMinutes, loaded: true };
        } catch (error) {
            Logger.error('[combined-today] Failed to load combined daily total', error);
            this.state = { ...this.state, loaded: true };
        }
        this.render();
    }

    render(): void {
        const { immersionMinutes, appMinutes, loaded } = this.state;
        const total = immersionMinutes + appMinutes;
        const root = html`
            <div class="combined-today-card">
                <span class="combined-today-label">Japanese time today</span>
                <span class="combined-today-value">${loaded ? formatMinutes(total) : '…'}</span>
                <div class="combined-today-breakdown">
                    <span class="combined-today-part">
                        <span class="combined-today-dot combined-today-dot--immersion"></span>
                        Immersion ${formatMinutes(immersionMinutes)}
                    </span>
                    <span class="combined-today-part">
                        <span class="combined-today-dot combined-today-dot--apps"></span>
                        Apps ${formatMinutes(appMinutes)}
                    </span>
                </div>
            </div>
        `;
        this.clear();
        this.container.appendChild(root);
    }
}
