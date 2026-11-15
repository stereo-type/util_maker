//! Плагины-расширения на стороне оболочки: маршрутизация `extensions.*` в
//! ядро, страница настроек родителя и миграция старых профилей переводчика.

use std::sync::{Arc, Mutex};

use serde::Serialize;
use serde_json::Value;
use util_maker_core::registry::interface_matches;
use util_maker_core::{ExtensionRouter, HostDispatcher, Registry};
use util_maker_platform::{NativeHost, NativeSelection, SettingsStore};
use util_maker_sdk::{error_codes, ExtensionInvokeRequest, ExtensionListParams, RpcError};

const TRANSLATOR_ID: &str = "org.utilmaker.translator";
const TRANSLATION_POINT: &str = "translation-provider";

/// Обслуживает `extensions.list` и `extension.invoke` для одного вызова.
pub struct ExtensionRouterImpl {
    registry: Arc<Registry>,
    settings: Arc<SettingsStore>,
    selection: Arc<Mutex<NativeSelection>>,
}

impl ExtensionRouterImpl {
    pub fn new(
        registry: Arc<Registry>,
        settings: Arc<SettingsStore>,
        selection: Arc<Mutex<NativeSelection>>,
    ) -> Self {
        Self {
            registry,
            settings,
            selection,
        }
    }
}

impl ExtensionRouter for ExtensionRouterImpl {
    fn list(
        &mut self,
        parent: &util_maker_core::CallSession,
        params: &Value,
    ) -> Result<Value, RpcError> {
        let params: ExtensionListParams =
            serde_json::from_value(params.clone()).map_err(|error| {
                RpcError::new(
                    error_codes::INVALID_PARAMS,
                    format!("параметры extensions.list: {error}"),
                )
            })?;
        let result = self
            .registry
            .extension_list(&parent.module_id, &params.point_id)?;
        serde_json::to_value(result).map_err(|error| {
            RpcError::new(
                error_codes::INTERNAL_ERROR,
                format!("ответ extensions.list: {error}"),
            )
        })
    }

    fn invoke(
        &mut self,
        parent: &util_maker_core::CallSession,
        params: &Value,
    ) -> Result<Value, RpcError> {
        let request: ExtensionInvokeRequest =
            serde_json::from_value(params.clone()).map_err(|error| {
                RpcError::new(
                    error_codes::INVALID_PARAMS,
                    format!("параметры extension.invoke: {error}"),
                )
            })?;
        let mut child = HostDispatcher::new(NativeHost::new(
            self.selection.clone(),
            self.settings.clone(),
        ));
        let result = self.registry.invoke_extension(
            parent,
            &request,
            Some(crate::logging::logger()),
            &mut child,
        )?;
        serde_json::to_value(result).map_err(|error| {
            RpcError::new(
                error_codes::INTERNAL_ERROR,
                format!("ответ extension.invoke: {error}"),
            )
        })
    }
}

/// Пакет-расширение в общем менеджере пакетов.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtensionPackageDto {
    pub module_id: String,
    pub name: String,
    pub version: String,
    pub publisher: String,
    pub enabled: bool,
    pub parent_id: String,
    pub parent_name: Option<String>,
    pub point_id: String,
    pub contract: String,
    pub interface_version: String,
    /// `available`, `parent-missing`, `parent-disabled`, `point-missing`,
    /// `incompatible` или `disabled`.
    pub state: String,
    pub reason: Option<String>,
}

/// Собирает все вклады установленных пакетов, включая пакеты без действий.
pub fn extension_packages(registry: &Registry) -> Vec<ExtensionPackageDto> {
    let modules = registry.modules();
    let mut result = Vec::new();
    for module in &modules {
        for contribution in &module.manifest.provides {
            let parent = modules
                .iter()
                .find(|entry| entry.manifest.id == contribution.module_id);
            let point =
                parent.and_then(|entry| entry.manifest.extension_point(&contribution.point_id));
            let (state, reason) = match (parent, point) {
                (None, _) => (
                    "parent-missing",
                    Some(format!(
                        "Микроприложение {} не установлено",
                        contribution.module_id
                    )),
                ),
                (Some(_), None) => (
                    "point-missing",
                    Some(format!(
                        "Точка {} не объявлена микроприложением",
                        contribution.point_id
                    )),
                ),
                (Some(parent), Some(point)) => {
                    if contribution.contract != point.contract
                        || !interface_matches(
                            &contribution.interface_version,
                            &point.contract_version,
                        )
                    {
                        (
                            "incompatible",
                            Some(format!(
                                "Нужен контракт {} {}, пакет объявляет {} {}",
                                point.contract,
                                point.contract_version,
                                contribution.contract,
                                contribution.interface_version
                            )),
                        )
                    } else if !parent.enabled {
                        (
                            "parent-disabled",
                            Some("Микроприложение отключено".to_owned()),
                        )
                    } else if !module.enabled {
                        ("disabled", Some("Пакет отключён".to_owned()))
                    } else {
                        ("available", None)
                    }
                }
            };
            result.push(ExtensionPackageDto {
                module_id: module.manifest.id.clone(),
                name: contribution.name.clone(),
                version: module.manifest.version.clone(),
                publisher: module.manifest.publisher.clone(),
                enabled: module.enabled,
                parent_id: contribution.module_id.clone(),
                parent_name: parent.map(|entry| entry.manifest.name.clone()),
                point_id: contribution.point_id.clone(),
                contract: contribution.contract.clone(),
                interface_version: contribution.interface_version.clone(),
                state: state.to_owned(),
                reason,
            });
        }
    }
    result.sort_by(|left, right| {
        left.parent_id
            .cmp(&right.parent_id)
            .then_with(|| left.name.cmp(&right.name))
    });
    result
}

/// Переносит старый профиль переводчика на плагины провайдеров: копирует
/// ключи под теми же ссылками в настройки плагина и выбирает активный вклад.
pub fn migrate_translator_profile(
    registry: &Registry,
    settings: &SettingsStore,
) -> Result<(), String> {
    if registry.module(TRANSLATOR_ID).is_none() {
        return Ok(());
    }
    let values = settings
        .values(TRANSLATOR_ID, None)
        .map_err(|error| format!("настройки переводчика: {error}"))?;
    let active = values
        .get("activeProfile")
        .and_then(Value::as_str)
        .unwrap_or("yandex");

    for kind in ["yandex", "google"] {
        let secret_ref = format!("translator/{kind}/apiKey");
        let plugin_id = format!("{TRANSLATOR_ID}.{kind}");
        if registry.module(&plugin_id).is_none() {
            continue;
        }
        if settings
            .secret_present(&plugin_id, &secret_ref)
            .unwrap_or(false)
        {
            continue;
        }
        if let Ok(Some(value)) = settings.read_secret(TRANSLATOR_ID, &secret_ref) {
            let _ = settings.write_secret(&plugin_id, &secret_ref, &value);
        }
    }

    if active != "yandex" && active != "google" {
        return Ok(());
    }
    let plugin_id = format!("{TRANSLATOR_ID}.{active}");
    let list = match registry.extension_list(TRANSLATOR_ID, TRANSLATION_POINT) {
        Ok(list) => list,
        Err(_) => return Ok(()),
    };
    if list.selected.is_some() {
        return Ok(());
    }
    let compatible = list
        .contributions
        .iter()
        .any(|item| item.module_id == plugin_id && item.available);
    if compatible {
        registry
            .select_extension(TRANSLATOR_ID, TRANSLATION_POINT, &plugin_id)
            .map_err(|error| format!("выбор провайдера: {error}"))?;
        crate::logging::write(&format!(
            "переводчик: активный профиль {active} перенесён на плагин {plugin_id}"
        ));
    }
    Ok(())
}
