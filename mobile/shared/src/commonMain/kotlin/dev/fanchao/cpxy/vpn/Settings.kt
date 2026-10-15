package dev.fanchao.cpxy.vpn

import androidx.datastore.core.DataStore
import androidx.datastore.preferences.core.PreferenceDataStoreFactory
import androidx.datastore.preferences.core.Preferences
import androidx.datastore.preferences.core.edit
import androidx.datastore.preferences.core.stringPreferencesKey
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.map
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.Json
import okio.Path

/** Which apps the VPN carries. */
@Serializable
enum class AppFilter {
    AllApps,

    /** Only [AppSettings.onlyApps] use the VPN (an allowlist). */
    OnlySelected,

    /** [AppSettings.exceptApps] bypass the VPN (a blocklist). */
    ExceptSelected,
}

/** Settings that apply to every profile. Each list keeps its apps while the other filter is used. */
@Serializable
data class AppSettings(
    val appFilter: AppFilter = AppFilter.AllApps,
    /** Package names, for [AppFilter.OnlySelected]. */
    val onlyApps: Set<String> = emptySet(),
    /** Package names, for [AppFilter.ExceptSelected]. */
    val exceptApps: Set<String> = emptySet(),
    val httpProxyEnabled: Boolean = false,
    val httpProxyPort: Int = 8080,
    val socksProxyEnabled: Boolean = false,
    val socksProxyPort: Int = 1080,
    /** IPv4 CIDRs forced through the main proxy for every profile. */
    val proxyRoutes: List<String> = emptyList(),
    /** Ordered search suffixes for all profiles. */
    val dnsSearchDomains: List<String> = emptyList(),
    /** When [UpdateChecker] last asked the server, in epoch milliseconds. */
    val lastUpdateCheck: Long = 0,
    /** A version the user chose not to install; the daily check does not offer it again. */
    val skippedUpdate: String? = null,
) {
    init {
        require(dnsSearchDomains.all(::isDnsSearchDomain)) { "Search domains must be DNS hostname suffixes" }
        require(proxyRoutes.all(::isIpv4Cidr)) { "Routes must be IPv4 CIDRs, such as 203.0.113.0/24" }
        require(httpProxyPort in 1..65535 && socksProxyPort in 1..65535) { "Ports must be between 1 and 65535" }
        require(!httpProxyEnabled || !socksProxyEnabled || httpProxyPort != socksProxyPort) { "HTTP and SOCKS ports must be different" }
    }

    /** The apps [filter] applies to. */
    fun appsFor(filter: AppFilter): Set<String> = when (filter) {
        AppFilter.AllApps -> emptySet()
        AppFilter.OnlySelected -> onlyApps
        AppFilter.ExceptSelected -> exceptApps
    }

    fun withApps(filter: AppFilter, apps: Set<String>): AppSettings = when (filter) {
        AppFilter.AllApps -> this
        AppFilter.OnlySelected -> copy(onlyApps = apps)
        AppFilter.ExceptSelected -> copy(exceptApps = apps)
    }

    /**
     * The apps the VPN carries, leaving out [ownPackage] so the engine's own sockets bypass it.
     * An allowlist with no apps carries every app, as the platforms do with an empty one.
     */
    fun routing(ownPackage: String): AppRouting {
        val only = onlyApps - ownPackage
        return if (appFilter == AppFilter.OnlySelected && only.isNotEmpty()) {
            AppRouting.Only(only)
        } else {
            AppRouting.Except(
                if (appFilter == AppFilter.ExceptSelected) exceptApps + ownPackage else setOf(ownPackage),
            )
        }
    }
}

sealed interface AppRouting {
    data class Only(val packages: Set<String>) : AppRouting
    data class Except(val packages: Set<String>) : AppRouting
}

/** Keeps the [AppSettings] as one JSON value in a preferences DataStore. */
class SettingsRepository(private val dataStore: DataStore<Preferences>) {
    val settings: Flow<AppSettings> = dataStore.data.map { decode(it[SettingsKey]) }

    suspend fun update(transform: (AppSettings) -> AppSettings) {
        dataStore.edit { preferences ->
            preferences[SettingsKey] = json.encodeToString(transform(decode(preferences[SettingsKey])))
        }
    }

    private fun decode(raw: String?): AppSettings =
        raw?.let { json.decodeFromString<AppSettings>(it) } ?: AppSettings()

    companion object {
        private val SettingsKey = stringPreferencesKey("settings_json")
        private val json = Json { ignoreUnknownKeys = true }

        fun create(path: Path, scope: CoroutineScope): SettingsRepository {
            require(path.name.endsWith(".preferences_pb")) {
                "DataStore path must end with .preferences_pb: $path"
            }
            return SettingsRepository(
                PreferenceDataStoreFactory.createWithPath(scope = scope, produceFile = { path })
            )
        }
    }
}

/** Validate literal IPv4 CIDRs without performing DNS lookups. */
fun isIpv4Cidr(value: String): Boolean {
    val parts = value.split('/')
    if (parts.size != 2) return false
    val prefix = parts[1]
    if (prefix.isEmpty() || prefix.any { it !in '0'..'9' } || prefix.toIntOrNull() !in 0..32) return false
    val octets = parts[0].split('.')
    return octets.size == 4 && octets.all {
        it.isNotEmpty() && it.length <= 3 && it.all { char -> char in '0'..'9' } &&
            (it.length == 1 || it[0] != '0') && it.toIntOrNull() in 0..255
    }
}

/** One CIDR per line; blank lines are ignored and duplicates are removed. */
fun parseProxyRoutes(text: String): List<String> =
    text.lines().map(String::trim).filter(String::isNotEmpty).distinct()

/** ASCII DNS hostname suffix, optionally ending in a dot; no root, IPs or wildcards. */
fun isDnsSearchDomain(value: String): Boolean {
    val domain = value.removeSuffix(".")
    if (domain.isEmpty() || domain.length > 253) return false
    val labels = domain.split('.')
    if (labels.size == 4 && labels.all { it.toIntOrNull() in 0..255 }) return false
    return labels.all { label ->
        label.isNotEmpty() && label.length <= 63 && !label.startsWith('-') && !label.endsWith('-') &&
            label.all { it in 'a'..'z' || it in 'A'..'Z' || it in '0'..'9' || it == '-' }
    }
}

/** One suffix per line, preserving priority and removing duplicate spellings. */
fun parseDnsSearchDomains(text: String): List<String> =
    text.lines().map(String::trim).filter(String::isNotEmpty)
        .map { if (isDnsSearchDomain(it)) it.removeSuffix(".").lowercase() else it }.distinct()
