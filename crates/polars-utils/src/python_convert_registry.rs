use std::any::Any;
use std::ops::Deref;
use std::sync::{Arc, LazyLock, RwLock};

use pyo3::{Py, PyAny, PyResult};

pub type PythonToPartitionTarget = Arc<dyn Fn(Py<PyAny>) -> PyResult<Box<dyn Any>> + Send + Sync>;
pub type DataFrameToPython = Arc<dyn Fn(Box<dyn Any>) -> PyResult<Py<PyAny>> + Send + Sync>;

#[derive(Clone)]
pub struct FromPythonConvertRegistry {
    pub partition_target_cb_result: PythonToPartitionTarget,
}

#[derive(Clone)]
pub struct ToPythonConvertRegistry {
    pub df: DataFrameToPython,
}

#[derive(Clone)]
pub struct PythonConvertRegistry {
    pub from_py: FromPythonConvertRegistry,
    pub to_py: ToPythonConvertRegistry,
}

static PYTHON_CONVERT_REGISTRY: LazyLock<RwLock<Option<PythonConvertRegistry>>> =
    LazyLock::new(Default::default);

pub fn get_python_convert_registry() -> PythonConvertRegistry {
    PYTHON_CONVERT_REGISTRY
        .deref()
        .read()
        .unwrap()
        .as_ref()
        .unwrap()
        .clone()
}

pub fn register_converters(registry: PythonConvertRegistry) {
    *PYTHON_CONVERT_REGISTRY.deref().write().unwrap() = Some(registry);
}
