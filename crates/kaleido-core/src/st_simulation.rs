//! 剧情因果推演校验（X1a）
//! 吞噬自 xiami story_simulation.rs（虾米剧情因果推演器）。
//! 纯数据结构 + 校验/渲染函数，无 LLM / IO。由 server 层负责 LLM 接线。

use serde::{Deserialize, Deserializer, Serialize};

pub const SYSTEM_PROMPT: &str = r#"你是长篇小说的因果推演器，不负责文风，也不写正文。依据正史、人物状态、大纲和本章任务，先模拟本章真实可发生的行动链。

只输出一个完整 JSON 对象，不要 Markdown 或解释。结构：
{
  "openingState": {
    "timeAndLocation": "开场精确时间与地点",
    "presentCharacters": ["在场人物及进入场景的依据"],
    "knowledgeBoundaries": ["人物：已知事实 / 未知事实 / 信息来源"],
    "resourcesAndConstraints": ["伤势、资源、权限、距离、期限、物理限制"]
  },
  "causalChain": [{
    "trigger": "触发事件",
    "actorAndGoal": "行动者及当下目标",
    "availableOptions": ["基于其认知和资源真实可选的行动"],
    "choiceAndReason": "最终选择及符合人物利益/性格的理由",
    "directResult": "直接结果",
    "cost": "代价或风险，不可为空",
    "secondOrderEffect": "对之后人物、关系、资源、风险或计划的影响"
  }],
  "feasibility": {
    "timeline": "每段行动所需时间及衔接",
    "distanceAndMovement": "移动路径和距离是否允许",
    "informationSources": "每个关键判断的信息从何而来",
    "authorityAndResources": "权限和资源来源、消耗及剩余",
    "physicalAndWorldRules": "伤势、能力、制度和世界规则约束",
    "coincidenceCheck": "是否靠巧合、降智或临时规则；若有则改写因果链"
  },
  "endingState": "本章结束时已经改变的可结算状态",
  "unresolvedPressures": ["仍未解决且不得擅自坐实的问题"],
  "nextImpetus": "由本章结果自然产生的下一章推动力"
}

硬约束：行动只能来自人物已有认知、动机、资源与权限；信息必须有来源；时间、距离、伤势和资源必须连续；反派不能为主角便利而失误；冲突不能靠巧合、降智、突然能力或临时世界规则解决；大纲是计划，若与已发布正史冲突必须服从正史。除非正文确实无法推进，否则本章必须设计至少两类状态变化，并且至少一类不可逆：目标、资源/权限、关系/立场、认知、新信息、伏笔代价、伤势/位置/风险，或旧计划失败后形成的新任务。只换地点、编号、设备、措辞不算变化；不得连续使用“继续调查、等待机会、确认线索”等占位任务。endingState 必须写明本章选择造成的新状态，nextImpetus 必须是下一章必须处理的新问题。文风要求不属于本阶段，不得添加。"#;

/// 数组或单值兼容反序列化（吞噬自 xiami ai_json.rs vec_or_single）
pub fn vec_or_single<'de, T, D>(deserializer: D) -> Result<Vec<T>, D::Error>
where
    T: Deserialize<'de>,
    D: Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum VecOrSingle<T> {
        Vec(Vec<T>),
        Single(T),
    }
    Ok(match VecOrSingle::deserialize(deserializer)? {
        VecOrSingle::Vec(values) => values,
        VecOrSingle::Single(value) => vec![value],
    })
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
pub struct OpeningState {
    pub time_and_location: String,
    #[serde(deserialize_with = "vec_or_single")]
    pub present_characters: Vec<String>,
    #[serde(deserialize_with = "vec_or_single")]
    pub knowledge_boundaries: Vec<String>,
    #[serde(deserialize_with = "vec_or_single")]
    pub resources_and_constraints: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
pub struct CausalBeat {
    pub trigger: String,
    pub actor_and_goal: String,
    #[serde(deserialize_with = "vec_or_single")]
    pub available_options: Vec<String>,
    pub choice_and_reason: String,
    pub direct_result: String,
    pub cost: String,
    pub second_order_effect: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
pub struct FeasibilityReview {
    pub timeline: String,
    pub distance_and_movement: String,
    pub information_sources: String,
    pub authority_and_resources: String,
    pub physical_and_world_rules: String,
    pub coincidence_check: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
pub struct ChapterSimulation {
    pub opening_state: OpeningState,
    #[serde(deserialize_with = "vec_or_single")]
    pub causal_chain: Vec<CausalBeat>,
    pub feasibility: FeasibilityReview,
    pub ending_state: String,
    #[serde(deserialize_with = "vec_or_single")]
    pub unresolved_pressures: Vec<String>,
    pub next_impetus: String,
}

/// 判定为「可感知状态变化」的语义标记
const MEANINGFUL_MARKERS: [&str; 22] = [
    "改变",
    "失去",
    "获得",
    "拿到",
    "发现",
    "意识",
    "暴露",
    "受伤",
    "离开",
    "交出",
    "承认",
    "转为",
    "决定",
    "拒绝",
    "同意",
    "失败",
    "成功",
    "代价",
    "关系",
    "权限",
    "风险",
    "新任务",
];

/// 不得连续使用的占位任务
const PLACEHOLDER_TASKS: [&str; 3] = ["继续调查", "等待机会", "确认线索"];

/// 归一化 + 完整性校验。
/// 校验通过返回 `Ok(())`，否则返回可读的中文错误（模块内部用 `Result<(), String>`）。
pub fn validate_simulation(simulation: &mut ChapterSimulation) -> Result<(), String> {
    normalize(simulation);
    if simulation.opening_state.time_and_location.is_empty() {
        return Err("剧情推演缺少开场时间与地点".to_owned());
    }
    if simulation.opening_state.present_characters.is_empty()
        || simulation.opening_state.knowledge_boundaries.is_empty()
        || simulation
            .opening_state
            .resources_and_constraints
            .is_empty()
    {
        return Err("剧情推演缺少在场人物、认知边界或资源限制".to_owned());
    }
    if simulation.causal_chain.len() < 2 {
        return Err("剧情推演至少需要两个连续因果节拍".to_owned());
    }
    for (index, beat) in simulation.causal_chain.iter().enumerate() {
        if [
            beat.trigger.as_str(),
            beat.actor_and_goal.as_str(),
            beat.choice_and_reason.as_str(),
            beat.direct_result.as_str(),
            beat.cost.as_str(),
            beat.second_order_effect.as_str(),
        ]
        .iter()
        .any(|value| value.is_empty())
            || beat.available_options.is_empty()
        {
            return Err(format!("剧情推演第 {} 个因果节拍字段不完整", index + 1));
        }
    }
    if [
        simulation.feasibility.timeline.as_str(),
        simulation.feasibility.distance_and_movement.as_str(),
        simulation.feasibility.information_sources.as_str(),
        simulation.feasibility.authority_and_resources.as_str(),
        simulation.feasibility.physical_and_world_rules.as_str(),
        simulation.feasibility.coincidence_check.as_str(),
        simulation.ending_state.as_str(),
        simulation.next_impetus.as_str(),
    ]
    .iter()
    .any(|value| value.is_empty())
    {
        return Err("剧情推演缺少可行性检查或结束状态".to_owned());
    }
    let meaningful_beats = simulation
        .causal_chain
        .iter()
        .filter(|beat| {
            let text = format!(
                "{} {} {} {}",
                beat.choice_and_reason, beat.direct_result, beat.cost, beat.second_order_effect
            );
            MEANINGFUL_MARKERS.iter().any(|marker| text.contains(marker))
        })
        .count();
    if meaningful_beats < 2 {
        return Err("剧情推演缺少至少两类可感知的状态变化；不能只换地点或重复调查".to_owned());
    }
    if has_placeholder_task(simulation) {
        return Err(
            "剧情推演不得连续使用“继续调查、等待机会、确认线索”等占位任务".to_owned(),
        );
    }
    Ok(())
}

/// 检测是否存在仅由占位动作构成的因果节拍（无任何可感知状态变化标记）
fn has_placeholder_task(simulation: &ChapterSimulation) -> bool {
    simulation.causal_chain.iter().any(|beat| {
        let text = format!("{} {}", beat.choice_and_reason, beat.direct_result);
        PLACEHOLDER_TASKS.iter().any(|task| text.contains(task))
            && !MEANINGFUL_MARKERS.iter().any(|marker| text.contains(marker))
    })
}

/// 渲染校验后的推演骨架（仅作为正文因果骨架，禁止原样输出）
pub fn render(simulation: &ChapterSimulation) -> String {
    let beats = simulation
        .causal_chain
        .iter()
        .enumerate()
        .map(|(index, beat)| {
            format!(
                "{}. 触发：{}\n行动者与目标：{}\n可选行动：{}\n选择与理由：{}\n直接结果：{}\n代价：{}\n二阶影响：{}",
                index + 1,
                beat.trigger,
                beat.actor_and_goal,
                display(&beat.available_options),
                beat.choice_and_reason,
                beat.direct_result,
                beat.cost,
                beat.second_order_effect,
            )
        })
        .collect::<Vec<_>>()
        .join("\n\n");
    format!(
        "# 已校验剧情推演（只作为正文因果骨架，禁止原样输出）\n开场时间地点：{}\n在场人物：{}\n认知边界：{}\n资源与限制：{}\n\n## 因果链\n{}\n\n## 可行性结论\n时间线：{}\n移动与距离：{}\n信息来源：{}\n权限与资源：{}\n物理与世界规则：{}\n巧合/降智检查：{}\n\n结束状态：{}\n未解决压力：{}\n下一章推动力：{}",
        simulation.opening_state.time_and_location,
        display(&simulation.opening_state.present_characters),
        display(&simulation.opening_state.knowledge_boundaries),
        display(&simulation.opening_state.resources_and_constraints),
        beats,
        simulation.feasibility.timeline,
        simulation.feasibility.distance_and_movement,
        simulation.feasibility.information_sources,
        simulation.feasibility.authority_and_resources,
        simulation.feasibility.physical_and_world_rules,
        simulation.feasibility.coincidence_check,
        simulation.ending_state,
        display(&simulation.unresolved_pressures),
        simulation.next_impetus,
    )
}

fn normalize(simulation: &mut ChapterSimulation) {
    simulation.opening_state.time_and_location =
        trimmed(&simulation.opening_state.time_and_location);
    trim_list(&mut simulation.opening_state.present_characters);
    trim_list(&mut simulation.opening_state.knowledge_boundaries);
    trim_list(&mut simulation.opening_state.resources_and_constraints);
    simulation.causal_chain.retain(|beat| {
        !beat.trigger.trim().is_empty() || !beat.choice_and_reason.trim().is_empty()
    });
    for beat in &mut simulation.causal_chain {
        beat.trigger = trimmed(&beat.trigger);
        beat.actor_and_goal = trimmed(&beat.actor_and_goal);
        trim_list(&mut beat.available_options);
        beat.choice_and_reason = trimmed(&beat.choice_and_reason);
        beat.direct_result = trimmed(&beat.direct_result);
        beat.cost = trimmed(&beat.cost);
        beat.second_order_effect = trimmed(&beat.second_order_effect);
    }
    simulation.feasibility.timeline = trimmed(&simulation.feasibility.timeline);
    simulation.feasibility.distance_and_movement =
        trimmed(&simulation.feasibility.distance_and_movement);
    simulation.feasibility.information_sources =
        trimmed(&simulation.feasibility.information_sources);
    simulation.feasibility.authority_and_resources =
        trimmed(&simulation.feasibility.authority_and_resources);
    simulation.feasibility.physical_and_world_rules =
        trimmed(&simulation.feasibility.physical_and_world_rules);
    simulation.feasibility.coincidence_check = trimmed(&simulation.feasibility.coincidence_check);
    simulation.ending_state = trimmed(&simulation.ending_state);
    trim_list(&mut simulation.unresolved_pressures);
    simulation.next_impetus = trimmed(&simulation.next_impetus);
}

fn trim_list(values: &mut Vec<String>) {
    values.iter_mut().for_each(|value| *value = trimmed(value));
    values.retain(|value| !value.is_empty());
}

fn trimmed(value: &str) -> String {
    value.trim().to_owned()
}

fn display(values: &[String]) -> String {
    if values.is_empty() {
        "无".to_owned()
    } else {
        values.join("；")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const VALID_JSON: &str = r#"{
      "openingState":{"timeAndLocation":"当夜，仓库","presentCharacters":["沈砚"],"knowledgeBoundaries":["沈砚只知道封条异常"],"resourcesAndConstraints":["左肩受伤，只有手机"]},
      "causalChain":[
        {"trigger":"封条异常","actorAndGoal":"沈砚确认调包","availableOptions":["拍照","触碰证物"],"choiceAndReason":"先拍照，因为不能碰证物","directResult":"保留影像","cost":"暴露位置","secondOrderEffect":"对手开始清场"},
        {"trigger":"对手靠近","actorAndGoal":"沈砚保护证据","availableOptions":["撤离","硬拼"],"choiceAndReason":"撤到后门，因为肩伤不能硬拼","directResult":"暂时脱离","cost":"放弃追人","secondOrderEffect":"失去抓现行机会"}
      ],
      "feasibility":{"timeline":"十分钟内","distanceAndMovement":"仓库后门步行可达","informationSources":"亲眼观察和照片","authorityAndResources":"只有手机","physicalAndWorldRules":"肩伤限制对抗","coincidenceCheck":"不依赖巧合"},
      "endingState":"保住影像但暴露位置","unresolvedPressures":["对手仍在清场"],"nextImpetus":"查照片中的铅封来源"
    }"#;

    #[test]
    fn validates_complete_simulation() {
        let mut simulation: ChapterSimulation = serde_json::from_str(VALID_JSON).unwrap();
        validate_simulation(&mut simulation).unwrap();
    }

    #[test]
    fn renders_cost_and_information_checks_for_the_writer() {
        let mut simulation: ChapterSimulation = serde_json::from_str(VALID_JSON).unwrap();
        validate_simulation(&mut simulation).unwrap();
        let rendered = render(&simulation);
        assert!(rendered.contains("代价：暴露位置"));
        assert!(rendered.contains("信息来源：亲眼观察和照片"));
        assert!(rendered.contains("未解决压力：对手仍在清场"));
    }

    #[test]
    fn rejects_incomplete_causal_chain() {
        let mut simulation: ChapterSimulation = serde_json::from_str(r#"{
          "openingState":{"timeAndLocation":"当夜，仓库","presentCharacters":["沈砚"],"knowledgeBoundaries":["只知道封条异常"],"resourcesAndConstraints":["左肩受伤"]},
          "causalChain":[
            {"trigger":"发现封条异常","actorAndGoal":"沈砚","availableOptions":["检查"],"choiceAndReason":"检查","directResult":"无异常","cost":"时间","secondOrderEffect":"无"}
          ],
          "feasibility":{"timeline":"","distanceAndMovement":"","informationSources":"","authorityAndResources":"","physicalAndWorldRules":"","coincidenceCheck":""},
          "endingState":"","unresolvedPressures":[],"nextImpetus":""
        }"#)
        .unwrap();
        let err = validate_simulation(&mut simulation).unwrap_err();
        assert!(err.contains("至少需要两个"));
    }

    #[test]
    fn rejects_missing_fields() {
        let mut simulation: ChapterSimulation = serde_json::from_str(r#"{
          "openingState":{"timeAndLocation":"当夜，仓库","presentCharacters":["沈砚"],"knowledgeBoundaries":["只知道封条异常"],"resourcesAndConstraints":["左肩受伤"]},
          "causalChain":[
            {"trigger":"封条异常","actorAndGoal":"沈砚确认调包","availableOptions":["拍照","触碰证物"],"choiceAndReason":"先拍照，因为不能碰证物","directResult":"保留影像","cost":"暴露位置","secondOrderEffect":"对手开始清场"},
            {"trigger":"对手靠近","actorAndGoal":"沈砚保护证据","availableOptions":["撤离","硬拼"],"choiceAndReason":"撤到后门，因为肩伤不能硬拼","directResult":"暂时脱离","cost":"放弃追人","secondOrderEffect":"失去抓现行机会"}
          ],
          "feasibility":{"timeline":"十分钟内","distanceAndMovement":"仓库后门步行可达","informationSources":"亲眼观察和照片","authorityAndResources":"只有手机","physicalAndWorldRules":"肩伤限制对抗","coincidenceCheck":"不依赖巧合"},
          "endingState":"",
          "nextImpetus":"查照片中的铅封来源"
        }"#)
        .unwrap();
        let err = validate_simulation(&mut simulation).unwrap_err();
        assert!(err.contains("结束状态"));
    }

    #[test]
    fn accepts_single_value_instead_of_array() {
        let mut simulation: ChapterSimulation = serde_json::from_str(r#"{
          "openingState":{"timeAndLocation":"当夜，仓库","presentCharacters":"沈砚","knowledgeBoundaries":["只知道封条异常"],"resourcesAndConstraints":["左肩受伤"]},
          "causalChain":{
            "trigger":"封条异常","actorAndGoal":"沈砚确认调包","availableOptions":["拍照","触碰证物"],"choiceAndReason":"先拍照，因为不能碰证物","directResult":"保留影像","cost":"暴露位置","secondOrderEffect":"对手开始清场"
          },
          "feasibility":{"timeline":"十分钟内","distanceAndMovement":"仓库后门步行可达","informationSources":"亲眼观察和照片","authorityAndResources":"只有手机","physicalAndWorldRules":"肩伤限制对抗","coincidenceCheck":"不依赖巧合"},
          "endingState":"保住影像但暴露位置","nextImpetus":"查照片中的铅封来源"
        }"#)
        .unwrap();
        assert_eq!(
            simulation.opening_state.present_characters,
            vec!["沈砚".to_owned()]
        );
        assert_eq!(simulation.causal_chain.len(), 1);
        assert_eq!(simulation.causal_chain[0].available_options.len(), 2);
        let err = validate_simulation(&mut simulation).unwrap_err();
        assert!(err.contains("至少需要两个"));
    }

    #[test]
    fn rejects_placeholder_tasks() {
        let mut simulation: ChapterSimulation = serde_json::from_str(r#"{
          "openingState":{"timeAndLocation":"当夜，仓库","presentCharacters":["沈砚"],"knowledgeBoundaries":["只知道封条异常"],"resourcesAndConstraints":["左肩受伤"]},
          "causalChain":[
            {"trigger":"封条异常","actorAndGoal":"沈砚确认调包","availableOptions":["继续观察"],"choiceAndReason":"继续调查","directResult":"继续观察","cost":"时间","secondOrderEffect":"暂无"},
            {"trigger":"对手靠近","actorAndGoal":"沈砚保护证据","availableOptions":["撤离"],"choiceAndReason":"撤到后门","directResult":"失去追人机会","cost":"放弃追人","secondOrderEffect":"暴露位置"},
            {"trigger":"封条异常","actorAndGoal":"沈砚保留影像","availableOptions":["保留"],"choiceAndReason":"决定保留照片","directResult":"获得影像证据","cost":"暴露","secondOrderEffect":"对手清场"}
          ],
          "feasibility":{"timeline":"十分钟内","distanceAndMovement":"步行可达","informationSources":"亲眼观察","authorityAndResources":"只有手机","physicalAndWorldRules":"肩伤限制","coincidenceCheck":"不依赖巧合"},
          "endingState":"保住影像","nextImpetus":"查铅封来源"
        }"#)
        .unwrap();
        let err = validate_simulation(&mut simulation).unwrap_err();
        assert!(err.contains("占位任务"));
    }
}
