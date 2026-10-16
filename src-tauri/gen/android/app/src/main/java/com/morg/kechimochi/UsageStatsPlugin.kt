package com.morg.kechimochi

import android.app.Activity
import android.app.AppOpsManager
import android.app.usage.UsageStatsManager
import android.content.Context
import android.content.Intent
import android.os.Build
import android.os.Process
import android.provider.Settings
import app.tauri.annotation.Command
import app.tauri.annotation.InvokeArg
import app.tauri.annotation.TauriPlugin
import app.tauri.plugin.Invoke
import app.tauri.plugin.JSArray
import app.tauri.plugin.JSObject
import app.tauri.plugin.Plugin
import java.util.Calendar

@InvokeArg
class QueryUsageArgs {
  var packageNames: List<String> = emptyList()
  lateinit var startDate: String
  lateinit var endDate: String
}

/**
 * Tracked App Time — Android automatic collection bridge (soft-fork feature).
 *
 * Reads per-app foreground time from `UsageStatsManager` for the packages the
 * user chose to track, bucketed by local calendar day. It only ever queries the
 * packages passed in (spec §3.9 privacy discipline) and reports whether the
 * "Usage access" special permission is currently granted so the Rust side can
 * surface a revoked state (spec §3.8) rather than silently showing zeroes.
 */
@TauriPlugin
class UsageStatsPlugin(private val activity: Activity) : Plugin(activity) {

  private fun hasUsageAccess(): Boolean {
    val appOps = activity.getSystemService(Context.APP_OPS_SERVICE) as AppOpsManager
    val mode = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
      appOps.unsafeCheckOpNoThrow(
        AppOpsManager.OPSTR_GET_USAGE_STATS,
        Process.myUid(),
        activity.packageName,
      )
    } else {
      @Suppress("DEPRECATION")
      appOps.checkOpNoThrow(
        AppOpsManager.OPSTR_GET_USAGE_STATS,
        Process.myUid(),
        activity.packageName,
      )
    }
    return mode == AppOpsManager.MODE_ALLOWED
  }

  /** Local-midnight millis for a `YYYY-MM-DD` date (start of that calendar day). */
  private fun dayStartMillis(date: String): Long {
    val parts = date.split("-")
    val calendar = Calendar.getInstance()
    calendar.clear()
    calendar.set(parts[0].toInt(), parts[1].toInt() - 1, parts[2].toInt(), 0, 0, 0)
    return calendar.timeInMillis
  }

  @Command
  fun queryUsage(invoke: Invoke) {
    val args = invoke.parseArgs(QueryUsageArgs::class.java)
    val result = JSObject()

    if (!hasUsageAccess()) {
      result.put("permissionGranted", false)
      result.put("usages", JSArray())
      invoke.resolve(result)
      return
    }

    val usageStatsManager =
      activity.getSystemService(Context.USAGE_STATS_SERVICE) as UsageStatsManager
    val wanted = args.packageNames.toHashSet()
    val usages = JSArray()

    // Walk each local calendar day in [startDate, endDate] and aggregate the
    // foreground time of the requested packages for that day.
    var cursor = dayStartMillis(args.startDate)
    val endExclusive = dayStartMillis(args.endDate) + DAY_MS
    while (cursor < endExclusive) {
      val dayEnd = cursor + DAY_MS
      val aggregated = usageStatsManager.queryAndAggregateUsageStats(cursor, dayEnd)
      val dayLabel = labelForMillis(cursor)
      for (packageName in wanted) {
        val stats = aggregated[packageName] ?: continue
        val minutes = stats.totalTimeInForeground / 60_000L
        if (minutes > 0) {
          val entry = JSObject()
          entry.put("packageName", packageName)
          entry.put("date", dayLabel)
          entry.put("minutes", minutes)
          usages.put(entry)
        }
      }
      cursor = dayEnd
    }

    result.put("permissionGranted", true)
    result.put("usages", usages)
    invoke.resolve(result)
  }

  @Command
  fun openUsageAccessSettings(invoke: Invoke) {
    val intent = Intent(Settings.ACTION_USAGE_ACCESS_SETTINGS)
    intent.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
    activity.startActivity(intent)
    invoke.resolve()
  }

  private fun labelForMillis(millis: Long): String {
    val calendar = Calendar.getInstance()
    calendar.timeInMillis = millis
    val year = calendar.get(Calendar.YEAR)
    val month = calendar.get(Calendar.MONTH) + 1
    val day = calendar.get(Calendar.DAY_OF_MONTH)
    return String.format("%04d-%02d-%02d", year, month, day)
  }

  companion object {
    private const val DAY_MS = 24L * 60L * 60L * 1000L
  }
}
