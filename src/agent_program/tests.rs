use super::*;
use runtime::ActionKind;

#[path="tests_stage3c.rs"]
mod stage3c;
#[path="tests_stage4.rs"]
mod stage4;
#[path = "tests_stage5.rs"]
mod stage5;
#[path="tests_stage5_fixes.rs"]
mod stage5_fixes;
#[path = "tests_stage6.rs"]
mod stage6;
#[path="tests_stage7.rs"]
mod stage7;
#[path="tests_stage8.rs"]
mod stage8;
#[path = "tests_work_budget.rs"]
mod work_budget;
#[path = "tests_bindings.rs"]
mod bindings;

include!("parts/tests_core_1.inc.rs");
include!("parts/tests_core_2.inc.rs");
include!("parts/tests_core_3.inc.rs");
