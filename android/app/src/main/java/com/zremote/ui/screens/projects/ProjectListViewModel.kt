package com.zremote.ui.screens.projects

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import com.zremote.data.ConnectionManager
import com.zremote.sdk.FfiProject
import dagger.hilt.android.lifecycle.HiltViewModel
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch
import javax.inject.Inject

@HiltViewModel
class ProjectListViewModel @Inject constructor(
    private val connectionManager: ConnectionManager,
) : ViewModel() {

    private val _projects = MutableStateFlow<List<FfiProject>>(emptyList())
    val projects: StateFlow<List<FfiProject>> = _projects.asStateFlow()

    private val _isLoading = MutableStateFlow(false)
    val isLoading: StateFlow<Boolean> = _isLoading.asStateFlow()

    private val _error = MutableStateFlow<String?>(null)
    val error: StateFlow<String?> = _error.asStateFlow()

    private var currentHostId: String? = null

    fun loadProjects(hostId: String) {
        currentHostId = hostId
        refresh()
    }

    fun refresh() {
        val hostId = currentHostId ?: return
        val client = connectionManager.client ?: return
        viewModelScope.launch {
            _isLoading.value = true
            _error.value = null
            try {
                _projects.value = client.listProjects(hostId)
            } catch (e: Exception) {
                _error.value = e.message
            } finally {
                _isLoading.value = false
            }
        }
    }

    fun triggerScan() {
        val hostId = currentHostId ?: return
        val client = connectionManager.client ?: return
        viewModelScope.launch {
            try {
                client.triggerScan(hostId)
                refresh()
            } catch (e: Exception) {
                _error.value = e.message
            }
        }
    }

    fun triggerGitRefresh(projectId: String) {
        val client = connectionManager.client ?: return
        viewModelScope.launch {
            try {
                client.triggerGitRefresh(projectId)
                refresh()
            } catch (e: Exception) {
                _error.value = e.message
            }
        }
    }
}
