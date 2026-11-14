### Kass UI strings, section "insights" (converted from i18next).

insights-title = Insights
insights-title-app = Insights · { $app }
insights-period-label = Period
insights-period-today = Today
insights-period-7d = 7 days
insights-period-30d = 30 days
insights-period-all = All time
insights-previous-today = yesterday
insights-previous-7d = the 7 days before
insights-previous-30d = the 30 days before
insights-legend-current-today = Today
insights-legend-current-7d = This week
insights-legend-current-30d = Last 30 days
insights-legend-current-all = All time
insights-legend-previous-today = Yesterday
insights-legend-previous-7d = Week before
insights-legend-previous-30d = 30 days before
insights-stats-words = Words dictated
insights-stats-pace = Speaking pace
insights-stats-saved = Time saved
insights-stats-weekdays = Weekdays you dictated
insights-stats-hours = Hours you dictated
insights-stats-wpm = wpm
insights-stats-of = of { $count }
insights-stats-compared = Compared with { $period }:
insights-stats-no-change = No change
insights-stats-new = New
insights-stats-nothing-before = Nothing to compare
insights-stats-since = Since { $date }
insights-stats-first-dictation = Counted from your first dictation:
insights-stats-prev-words =
    { $count ->
        [one] { $formatted } word
       *[other] { $formatted } words
    }
insights-stats-prev-pace = { $count } wpm
insights-stats-no-pace = No pace yet
insights-stats-prev-saved = { $time } saved
insights-stats-prev-hours =
    { $count ->
        [one] { $count } hour
       *[other] { $count } hours
    }
insights-stats-weekend =
    { $count ->
        [one] +{ $count } weekend day
       *[other] +{ $count } weekend days
    }
insights-stats-weekend-tip = Weekend dictation:
insights-stats-weekend-day = { $day } · { $words }
insights-stats-weekend-more =
    { $count ->
        [one] and { $count } more day
       *[other] and { $count } more days
    }
insights-stats-weekdays-only = Weekdays only
insights-stats-no-weekend = No dictation on
insights-stats-no-weekend-any = No weekend dictation in this period
insights-chart-hour = Words per hour
insights-chart-day = Words per day
insights-chart-week = Words per week
insights-chart-words =
    { $count ->
        [one] { $formatted } word
       *[other] { $formatted } words
    }
insights-chart-no-dictation = No dictation
insights-chart-on = { $value } on { $date }
insights-chart-change = { $pct } vs { $period }
insights-chart-today = today
insights-chart-this-week = this week
insights-chart-week-of = Week of { $date }
insights-chart-label = { $title } in { $app }
insights-where-title = Where you dictate
insights-where-hint = click an app to see only its stats
insights-where-compare-title = { $app } compared with all apps
insights-where-compare-hint = click to go back to all apps
insights-where-share = { $pct } of words
insights-where-share-title = Share of your words
insights-where-share-sub = { $words } of { $total }
insights-where-pace-title = Pace vs all apps
insights-where-pace-sub = { $pace } vs { $all } wpm
insights-where-fix-title = Needed a fix
insights-where-fix-sub = all apps: { $pct }
insights-where-none = No dictation in this period.
insights-when-title = When you dictate
insights-when-fewer = fewer
insights-when-more = more words
insights-when-busiest = Busiest: { $slot }.
insights-when-weekends-quiet = Weekends are nearly empty.
insights-when-none = No dictation in this period.
insights-length-title = How long you talk
insights-length-hint = share of dictations
insights-length-median = median { $value }
insights-length-share = { $pct }% of dictations
insights-length-count =
    { $count ->
        [one] { $count } dictation, { $bin } long
       *[other] { $count } dictations, { $bin } long
    }
insights-length-most = Most dictations run { $bin }.
insights-length-band = The darker band holds the middle half.
insights-length-none = No dictation in this period.
insights-empty-title = No dictation yet
insights-empty-body = Your words, pace and time saved show up here once you dictate.
