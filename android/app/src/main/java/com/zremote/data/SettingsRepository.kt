package com.zremote.data

import androidx.datastore.core.DataStore
import androidx.datastore.preferences.core.Preferences
import androidx.datastore.preferences.core.edit
import androidx.datastore.preferences.core.stringPreferencesKey
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.map
import javax.inject.Inject
import javax.inject.Singleton

@Singleton
class SettingsRepository @Inject constructor(
    private val dataStore: DataStore<Preferences>,
) {
    private val serverUrlKey = stringPreferencesKey("server_url")

    val serverUrl: Flow<String> = dataStore.data.map { prefs ->
        prefs[serverUrlKey] ?: ""
    }

    suspend fun setServerUrl(url: String) {
        dataStore.edit { prefs ->
            prefs[serverUrlKey] = url
        }
    }
}
