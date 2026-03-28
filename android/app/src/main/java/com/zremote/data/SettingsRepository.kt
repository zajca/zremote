package com.zremote.data

import androidx.datastore.core.DataStore
import androidx.datastore.preferences.core.Preferences
import androidx.datastore.preferences.core.booleanPreferencesKey
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
    val serverUrl: Flow<String> = dataStore.data.map { prefs ->
        prefs[KEY_SERVER_URL] ?: ""
    }

    val notifyLoopCompletions: Flow<Boolean> = dataStore.data.map { it[KEY_NOTIFY_LOOP_COMPLETIONS] ?: true }
    val notifyLoopErrors: Flow<Boolean> = dataStore.data.map { it[KEY_NOTIFY_LOOP_ERRORS] ?: true }
    val notifyPermissionRequests: Flow<Boolean> = dataStore.data.map { it[KEY_NOTIFY_PERMISSION_REQUESTS] ?: true }
    val notifyTaskCompletions: Flow<Boolean> = dataStore.data.map { it[KEY_NOTIFY_TASK_COMPLETIONS] ?: true }
    val notifyTaskErrors: Flow<Boolean> = dataStore.data.map { it[KEY_NOTIFY_TASK_ERRORS] ?: true }
    val notifyHostDisconnections: Flow<Boolean> = dataStore.data.map { it[KEY_NOTIFY_HOST_DISCONNECTIONS] ?: true }

    suspend fun setServerUrl(url: String) {
        dataStore.edit { prefs -> prefs[KEY_SERVER_URL] = url }
    }

    suspend fun setNotifyLoopCompletions(enabled: Boolean) {
        dataStore.edit { it[KEY_NOTIFY_LOOP_COMPLETIONS] = enabled }
    }

    suspend fun setNotifyLoopErrors(enabled: Boolean) {
        dataStore.edit { it[KEY_NOTIFY_LOOP_ERRORS] = enabled }
    }

    suspend fun setNotifyPermissionRequests(enabled: Boolean) {
        dataStore.edit { it[KEY_NOTIFY_PERMISSION_REQUESTS] = enabled }
    }

    suspend fun setNotifyTaskCompletions(enabled: Boolean) {
        dataStore.edit { it[KEY_NOTIFY_TASK_COMPLETIONS] = enabled }
    }

    suspend fun setNotifyTaskErrors(enabled: Boolean) {
        dataStore.edit { it[KEY_NOTIFY_TASK_ERRORS] = enabled }
    }

    suspend fun setNotifyHostDisconnections(enabled: Boolean) {
        dataStore.edit { it[KEY_NOTIFY_HOST_DISCONNECTIONS] = enabled }
    }

    companion object {
        private val KEY_SERVER_URL = stringPreferencesKey("server_url")
        val KEY_NOTIFY_LOOP_COMPLETIONS = booleanPreferencesKey("notify_loop_completions")
        val KEY_NOTIFY_LOOP_ERRORS = booleanPreferencesKey("notify_loop_errors")
        val KEY_NOTIFY_PERMISSION_REQUESTS = booleanPreferencesKey("notify_permission_requests")
        val KEY_NOTIFY_TASK_COMPLETIONS = booleanPreferencesKey("notify_task_completions")
        val KEY_NOTIFY_TASK_ERRORS = booleanPreferencesKey("notify_task_errors")
        val KEY_NOTIFY_HOST_DISCONNECTIONS = booleanPreferencesKey("notify_host_disconnections")
    }
}
