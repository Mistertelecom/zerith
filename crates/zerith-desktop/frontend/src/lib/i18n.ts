// Reactive multi-language UI localization (English default, plus Português (BR),
// Español, Français, Deutsch, and 中文).
import { useEffect, useState } from "react";

export type Lang = "en" | "pt-BR" | "es" | "fr" | "de" | "zh";

export const LANG_OPTIONS: readonly (readonly [Lang, string])[] = [
  ["en", "English"],
  ["pt-BR", "Português (BR)"],
  ["es", "Español"],
  ["fr", "Français"],
  ["de", "Deutsch"],
  ["zh", "中文"],
] as const;

export const LANG_SHORT: Record<Lang, string> = {
  en: "EN",
  "pt-BR": "PT",
  es: "ES",
  fr: "FR",
  de: "DE",
  zh: "中文",
};

const LANG_KEY = "zerith.lang";
const LANG_EVENT = "zerith:lang";
const VALID_LANGS = new Set<string>(["en", "pt-BR", "es", "fr", "de", "zh"]);

export function getLang(): Lang {
  try {
    const raw = localStorage.getItem(LANG_KEY);
    if (raw && VALID_LANGS.has(raw)) return raw as Lang;
    return "en";
  } catch {
    return "en";
  }
}

export function setLang(lang: Lang): void {
  const clean: Lang = VALID_LANGS.has(lang) ? lang : "en";
  try {
    localStorage.setItem(LANG_KEY, clean);
  } catch {
    /* ignore storage errors */
  }
  if (typeof document !== "undefined") {
    document.documentElement.lang = clean;
  }
  if (typeof window !== "undefined") {
    window.dispatchEvent(new CustomEvent<Lang>(LANG_EVENT, { detail: clean }));
  }
}

export function useLang(): Lang {
  const [lang, setLangState] = useState<Lang>(getLang);
  useEffect(() => {
    const onCustom = (e: Event) => {
      const detail = (e as CustomEvent<Lang>).detail;
      setLangState(VALID_LANGS.has(detail) ? detail : getLang());
    };
    const onStorage = (e: StorageEvent) => {
      if (e.key === LANG_KEY) setLangState(getLang());
    };
    window.addEventListener(LANG_EVENT, onCustom);
    window.addEventListener("storage", onStorage);
    return () => {
      window.removeEventListener(LANG_EVENT, onCustom);
      window.removeEventListener("storage", onStorage);
    };
  }, []);
  return lang;
}

type Entry = {
  pt: string;
  en: string;
  es: string;
  fr: string;
  de: string;
  zh: string;
};

// Exact UI phrases across all Zerith screens (Portuguese, English, Spanish, French, German, Chinese)
const PHRASES: readonly Entry[] = [
  // Sidebar & Navigation
  { pt: "Nova conversa", en: "New chat", es: "Nueva conversación", fr: "Nouvelle discussion", de: "Neuer Chat", zh: "新对话" },
  { pt: "Buscar conversas", en: "Search chats", es: "Buscar conversaciones", fr: "Rechercher des discussions", de: "Chats durchsuchen", zh: "搜索对话" },
  { pt: "Conversas", en: "Chats", es: "Conversaciones", fr: "Discussions", de: "Chats", zh: "对话" },
  { pt: "Nada encontrado", en: "Nothing found", es: "No se encontró nada", fr: "Rien trouvé", de: "Nichts gefunden", zh: "未找到内容" },
  { pt: "Nenhuma conversa ainda", en: "No chats yet", es: "Aún no hay conversaciones", fr: "Aucune discussion", de: "Noch keine Chats", zh: "暂无对话" },
  { pt: "Hoje", en: "Today", es: "Hoy", fr: "Aujourd'hui", de: "Heute", zh: "今天" },
  { pt: "Ontem", en: "Yesterday", es: "Ayer", fr: "Hier", de: "Gestern", zh: "昨天" },
  { pt: "Últimos 7 dias", en: "Last 7 days", es: "Últimos 7 días", fr: "7 derniers jours", de: "Letzte 7 Tage", zh: "过去 7 天" },
  { pt: "Mais antigas", en: "Older", es: "Anteriores", fr: "Plus anciennes", de: "Älter", zh: "更早" },
  { pt: "Esperando você", en: "Waiting for you", es: "Esperando tu respuesta", fr: "En attente", de: "Wartet auf dich", zh: "等待您的确认" },
  { pt: "Respondendo", en: "Responding", es: "Respondiendo", fr: "Réponse en cours", de: "Antwortet", zh: "正在回复" },
  { pt: "respondendo", en: "responding", es: "respondiendo", fr: "réponse en cours", de: "antwortet", zh: "正在回复" },
  { pt: "Memória", en: "Memory", es: "Memoria", fr: "Mémoire", de: "Gedächtnis", zh: "记忆" },
  { pt: "pausada", en: "paused", es: "pausada", fr: "en pause", de: "pausiert", zh: "已暂停" },
  { pt: "Pausada", en: "Paused", es: "Pausada", fr: "En pause", de: "Pausiert", zh: "已暂停" },
  { pt: "ativa", en: "active", es: "activa", fr: "active", de: "aktiv", zh: "已启用" },
  { pt: "Ativa", en: "Active", es: "Activa", fr: "Active", de: "Aktiv", zh: "已启用" },
  { pt: "Retomar memória", en: "Resume memory", es: "Reanudar memoria", fr: "Reprendre la mémoire", de: "Gedächtnis fortsetzen", zh: "恢复记忆" },
  { pt: "Pausar memória", en: "Pause memory", es: "Pausar memoria", fr: "Mettre la mémoire en pause", de: "Gedächtnis pausieren", zh: "暂停记忆" },
  { pt: "Cotas", en: "Quotas", es: "Cuotas", fr: "Quotas", de: "Kontingente", zh: "配额" },
  { pt: "Automações", en: "Workflows", es: "Automatizaciones", fr: "Automatisations", de: "Workflows", zh: "自动化工作流" },
  { pt: "Bots Especialistas", en: "Specialist Bots", es: "Bots Especialistas", fr: "Bots Spécialistes", de: "Spezialisten-Bots", zh: "专家机器人" },
  { pt: "Nova conversa com bot", en: "New bot chat", es: "Nuevo chat con bot", fr: "Nouveau chat avec bot", de: "Neuer Bot-Chat", zh: "新建机器人对话" },
  { pt: "Buscar bots", en: "Search bots", es: "Buscar bots", fr: "Rechercher des bots", de: "Bots suchen", zh: "搜索机器人" },
  { pt: "Abrir Workflows & Automações", en: "Open Workflows & Automations", es: "Abrir Workflows y Automatizaciones", fr: "Ouvrir Workflows et Automatisations", de: "Workflows & Automatisierung öffnen", zh: "打开自动化工作流" },
  { pt: "Canais", en: "Channels", es: "Canales", fr: "Canaux", de: "Kanäle", zh: "通知渠道" },
  { pt: "Galeria de Templates", en: "Template Gallery", es: "Galería de plantillas", fr: "Galerie de modèles", de: "Vorlagen-Galerie", zh: "模板库" },
  { pt: "Ajustes", en: "Settings", es: "Ajustes", fr: "Paramètres", de: "Einstellungen", zh: "设置" },
  { pt: "Seções dos ajustes", en: "Settings sections", es: "Secciones de ajustes", fr: "Sections des paramètres", de: "Einstellungsbereiche", zh: "设置分类" },
  { pt: "para fechar", en: "to close", es: "para cerrar", fr: "pour fermer", de: "zum Schließen", zh: "关闭" },
  { pt: "Fechar ajustes", en: "Close settings", es: "Cerrar ajustes", fr: "Fermer les paramètres", de: "Einstellungen schließen", zh: "关闭设置" },
  { pt: "Carregando…", en: "Loading…", es: "Cargando…", fr: "Chargement…", de: "Laden…", zh: "加载中…" },
  { pt: "Abrindo…", en: "Opening…", es: "Abriendo…", fr: "Ouverture…", de: "Wird geöffnet…", zh: "正在打开…" },
  { pt: "Lendo…", en: "Loading…", es: "Leyendo…", fr: "Lecture…", de: "Wird gelesen…", zh: "读取中…" },

  // Hero & Main Window
  { pt: "Como posso ajudar?", en: "How can I help?", es: "¿Cómo puedo ayudar?", fr: "Comment puis-je aider ?", de: "Wie kann ich helfen?", zh: "有什么可以帮您？" },
  { pt: "Peça código, revisão ou um plano. As ferramentas rodam no seu computador e pedem aprovação.", en: "Ask for code, reviews, or infrastructure operations. Tools run locally on your machine with approval.", es: "Pide código, revisiones u operaciones de red. Las herramientas se ejecutan en tu equipo con aprobación.", fr: "Demandez du code, une revue ou un plan. Les outils s'exécutent sur votre machine avec approbation.", de: "Frage nach Code, Reviews oder Netzwerk-Aufgaben. Tools laufen lokal mit deiner Freigabe.", zh: "请求代码、审查或网络运维操作。工具在本地运行并需您的批准。" },
  { pt: "Entender este projeto", en: "Understand this project", es: "Entender este proyecto", fr: "Comprendre ce projet", de: "Dieses Projekt verstehen", zh: "了解此项目" },
  { pt: "Mapa da arquitetura e por onde começar", en: "Architecture map and where to start", es: "Mapa de arquitectura y por dónde empezar", fr: "Carte de l'architecture et par où commencer", de: "Architekturübersicht und Einstieg", zh: "架构图与入门指南" },
  { pt: "Revisar minhas mudanças", en: "Review my changes", es: "Revisar mis cambios", fr: "Vérifier mes modifications", de: "Meine Änderungen prüfen", zh: "审查我的更改" },
  { pt: "Bugs, riscos e o que faltou testar no diff atual", en: "Bugs, risks, and missing tests in the current diff", es: "Bugs, riesgos y pruebas faltantes en el diff actual", fr: "Bugs, risques et tests manquants dans le diff actuel", de: "Bugs, Risiken und fehlende Tests im aktuellen Diff", zh: "当前变更中的缺陷、风险与遗漏测试" },
  { pt: "Planejar uma mudança", en: "Plan a change", es: "Planificar un cambio", fr: "Planifier un changement", de: "Eine Änderung planen", zh: "规划变更" },
  { pt: "Passos e arquivos antes de editar qualquer coisa", en: "Steps and files before editing anything", es: "Pasos y archivos antes de editar nada", fr: "Étapes et fichiers avant toute modification", de: "Schritte und Dateien vor dem Bearbeiten", zh: "编辑前的步骤与文件清单" },
  { pt: "Achar e corrigir um problema", en: "Find and fix an issue", es: "Encontrar y corregir un problema", fr: "Trouver et corriger un problème", de: "Ein Problem finden und beheben", zh: "查找并修复问题" },
  { pt: "Reproduzir, isolar a causa e propor o conserto", en: "Reproduce, isolate root cause, and propose a fix", es: "Reproducir, aislar la causa y proponer la solución", fr: "Reproduire, isoler la cause et proposer le correctif", de: "Reproduzieren, Ursache isolieren und Lösung vorschlagen", zh: "复现、定位根因并提供修复方案" },
  { pt: "Ocultar painel lateral", en: "Hide side panel", es: "Ocultar panel lateral", fr: "Masquer le panneau latéral", de: "Seitenleiste ausblenden", zh: "隐藏侧边栏" },
  { pt: "Mostrar painel lateral", en: "Show side panel", es: "Mostrar panel lateral", fr: "Afficher le panneau latéral", de: "Seitenleiste einblenden", zh: "显示侧边栏" },
  { pt: "Exportar relatório / conversa em Markdown (.md)", en: "Export report / conversation as Markdown (.md)", es: "Exportar informe / conversación en Markdown (.md)", fr: "Exporter le rapport / la discussion en Markdown (.md)", de: "Bericht / Chat als Markdown (.md) exportieren", zh: "将报告 / 对话导出为 Markdown (.md)" },

  // Composer & Modes
  { pt: "Pergunte algo ao Zerith…", en: "Ask Zerith anything…", es: "Pregúntale algo a Zerith…", fr: "Demandez quelque chose à Zerith…", de: "Frag Zerith etwas…", zh: "向 Zerith 提问…" },
  { pt: "Mensagem", en: "Message", es: "Mensaje", fr: "Message", de: "Nachricht", zh: "消息" },
  { pt: "/ comandos  ·  @ arquivos", en: "/ commands  ·  @ files", es: "/ comandos  ·  @ archivos", fr: "/ commandes  ·  @ fichiers", de: "/ Befehle  ·  @ Dateien", zh: "/ 命令  ·  @ 文件" },
  { pt: "modo", en: "mode", es: "modo", fr: "mode", de: "Modus", zh: "模式" },
  { pt: "comandos", en: "commands", es: "comandos", fr: "commandes", de: "Befehle", zh: "命令" },
  { pt: "Comandos", en: "Commands", es: "Comandos", fr: "Commandes", de: "Befehle", zh: "命令" },
  { pt: "Arquivos do workspace", en: "Workspace files", es: "Archivos del workspace", fr: "Fichiers de l'espace de travail", de: "Workspace-Dateien", zh: "工作区文件" },
  { pt: "Permissões", en: "Permissions", es: "Permisos", fr: "Permissions", de: "Berechtigungen", zh: "权限" },
  { pt: "Padrão", en: "Default", es: "Predeterminado", fr: "Par défaut", de: "Standard", zh: "默认" },
  { pt: "Aceitar edições", en: "Accept edits", es: "Aceptar ediciones", fr: "Accepter les modifications", de: "Edits akzeptieren", zh: "自动接受编辑" },
  { pt: "Plano", en: "Plan", es: "Plan", fr: "Plan", de: "Plan", zh: "计划" },
  { pt: "Bypass", en: "Bypass", es: "Bypass", fr: "Bypass", de: "Bypass", zh: "免确认 (Bypass)" },
  { pt: "Esforço", en: "Effort", es: "Esfuerzo", fr: "Effort", de: "Denkaufwand", zh: "推理强度" },
  { pt: "Off (padrão)", en: "Off (default)", es: "Off (predeterminado)", fr: "Off (défaut)", de: "Aus (Standard)", zh: "关闭 (默认)" },
  { pt: "Baixo", en: "Low", es: "Bajo", fr: "Faible", de: "Niedrig", zh: "低" },
  { pt: "Médio", en: "Medium", es: "Medio", fr: "Moyen", de: "Mittel", zh: "中" },
  { pt: "Alto", en: "High", es: "Alto", fr: "Élevé", de: "Hoch", zh: "高" },
  { pt: "Orquestrador", en: "Orchestrator", es: "Orquestador", fr: "Orchestrateur", de: "Orchestrator", zh: "编排模式" },
  { pt: "Pensamento", en: "Think", es: "Pensamiento", fr: "Réflexion", de: "Denken", zh: "思考模型" },
  { pt: "Execução", en: "Exec", es: "Ejecución", fr: "Exécution", de: "Ausführung", zh: "执行模型" },
  { pt: "Executor", en: "Executor", es: "Ejecutor", fr: "Exécuteur", de: "Ausführer", zh: "执行器" },
  { pt: "Modelo", en: "Model", es: "Modelo", fr: "Modèle", de: "Modell", zh: "模型" },
  { pt: "Escolha um modelo", en: "Choose a model", es: "Elige un modelo", fr: "Choisir un modèle", de: "Modell wählen", zh: "选择模型" },
  { pt: "Escolha o executor", en: "Choose executor", es: "Elige el ejecutor", fr: "Choisir l'exécuteur", de: "Ausführer wählen", zh: "选择执行模型" },
  { pt: "Buscar modelo ou conta…", en: "Search model or account…", es: "Buscar modelo o cuenta…", fr: "Rechercher un modèle ou compte…", de: "Modell oder Konto suchen…", zh: "搜索模型或账户…" },
  { pt: "Esconder ocultos", en: "Hide hidden", es: "Ocultar ocultos", fr: "Masquer les cachés", de: "Ausgeblendete verbergen", zh: "隐藏已折叠项" },
  { pt: "Enviar", en: "Send", es: "Enviar", fr: "Envoyer", de: "Senden", zh: "发送" },
  { pt: "Parar", en: "Stop", es: "Detener", fr: "Arrêter", de: "Stopp", zh: "停止" },
  { pt: "Direcionar", en: "Steer", es: "Dirigir", fr: "Orienter", de: "Lenken", zh: "引导" },
  { pt: "Resumir o histórico para caber no contexto", en: "Summarize history to fit context window", es: "Resumir el historial para caber en el contexto", fr: "Résumer l'historique pour tenir dans le contexte", de: "Verlauf zusammenfassen", zh: "压缩历史记录以适应上下文" },
  { pt: "Quanto do contexto já foi usado", en: "How much context window is used", es: "Cuánto contexto se ha usado", fr: "Quantité de contexte utilisée", de: "Genutztes Kontextfenster", zh: "查看上下文使用量" },
  { pt: "Voltar ao último ponto de restauração", en: "Rewind to last checkpoint", es: "Volver al último punto de restauración", fr: "Revenir au dernier point de contrôle", de: "Zum letzten Checkpoint zurückkehren", zh: "回滚到上一个检查点" },
  { pt: "Entrar em modo plano", en: "Enter plan mode", es: "Entrar en modo plan", fr: "Passer en mode plan", de: "In den Planmodus wechseln", zh: "进入计划模式" },
  { pt: "Trocar o modo de permissões", en: "Switch permission mode", es: "Cambiar modo de permisos", fr: "Changer le mode de permissions", de: "Berechtigungsmodus wechseln", zh: "切换权限模式" },
  { pt: "Escolher o modelo", en: "Choose model", es: "Elegir modelo", fr: "Choisir le modèle", de: "Modell auswählen", zh: "选择模型" },
  { pt: "Começar uma conversa nova", en: "Start a new chat", es: "Iniciar nueva conversación", fr: "Nouvelle discussion", de: "Neuen Chat starten", zh: "开始新对话" },
  { pt: "Abrir o histórico", en: "Open history", es: "Abrir historial", fr: "Ouvrir l'historique", de: "Verlauf öffnen", zh: "打开历史记录" },
  { pt: "Ver os agentes", en: "View agents", es: "Ver agentes", fr: "Voir les agents", de: "Agenten anzeigen", zh: "查看智能体" },
  { pt: "Ver as skills", en: "View skills", es: "Ver skills", fr: "Voir les skills", de: "Skills anzeigen", zh: "查看技能" },
  { pt: "Ver a memória", en: "View memory", es: "Ver memoria", fr: "Voir la mémoire", de: "Gedächtnis anzeigen", zh: "查看记忆" },
  { pt: "Pasta da conversa", en: "Conversation folder", es: "Carpeta de la conversación", fr: "Dossier de la discussion", de: "Chat-Ordner", zh: "对话工作目录" },
  { pt: "Recentes", en: "Recent", es: "Recientes", fr: "Récents", de: "Zuletzt verwendet", zh: "最近使用" },
  { pt: "Escolher outra pasta…", en: "Choose another folder…", es: "Elegir otra carpeta…", fr: "Choisir un autre dossier…", de: "Anderen Ordner wählen…", zh: "选择其他文件夹…" },
  { pt: "Abrir no explorador de arquivos", en: "Reveal in file explorer", es: "Abrir en el explorador de archivos", fr: "Ouvrir dans l'explorateur", de: "Im Datei-Explorer öffnen", zh: "在文件管理器中打开" },

  // Inspector & Right Panel
  { pt: "Detalhes da conversa", en: "Conversation details", es: "Detalles de la conversación", fr: "Détails de la discussion", de: "Chat-Details", zh: "对话详情" },
  { pt: "Subagentes", en: "Subagents", es: "Subagentes", fr: "Sous-agents", de: "Subagenten", zh: "子智能体" },
  { pt: "Tarefas", en: "Tasks", es: "Tareas", fr: "Tâches", de: "Aufgaben", zh: "任务" },
  { pt: "Rota", en: "Route", es: "Ruta", fr: "Route", de: "Route", zh: "路由" },
  { pt: "em uso", en: "in use", es: "en uso", fr: "en cours", de: "aktiv", zh: "使用中" },
  { pt: "próximo turno", en: "next turn", es: "próximo turno", fr: "prochain tour", de: "nächster Schritt", zh: "下一轮" },
  { pt: "Nenhum modelo escolhido.", en: "No model selected.", es: "Ningún modelo seleccionado.", fr: "Aucun modèle sélectionné.", de: "Kein Modell ausgewählt.", zh: "未选择模型。" },
  { pt: "Tokens", en: "Tokens", es: "Tokens", fr: "Tokens", de: "Tokens", zh: "Token 统计" },
  { pt: "sessão", en: "session", es: "sesión", fr: "session", de: "Sitzung", zh: "会话" },
  { pt: "Entrada", en: "Input", es: "Entrada", fr: "Entrée", de: "Eingabe", zh: "输入" },
  { pt: "Saída", en: "Output", es: "Salida", fr: "Sortie", de: "Ausgabe", zh: "输出" },
  { pt: "Cache", en: "Cache", es: "Caché", fr: "Cache", de: "Cache", zh: "缓存" },
  { pt: "Economia", en: "Savings", es: "Ahorro", fr: "Économie", de: "Ersparnis", zh: "节省" },
  { pt: "Total", en: "Total", es: "Total", fr: "Total", de: "Gesamt", zh: "总计" },
  { pt: "Comandos rápidos", en: "Quick commands", es: "Comandos rápidos", fr: "Commandes rapides", de: "Schnellbefehle", zh: "快捷命令" },
  { pt: "Digite um comando ou ação…", en: "Type a command or action…", es: "Escribe un comando o acción…", fr: "Tapez une commande ou action…", de: "Befehl oder Aktion eingeben…", zh: "输入命令或操作…" },
  { pt: "Nenhum comando encontrado", en: "No command found", es: "Ningún comando encontrado", fr: "Aucune commande trouvée", de: "Kein Befehl gefunden", zh: "未找到命令" },

  // Settings Navigation & Headers
  { pt: "Aplicativo", en: "Application", es: "Aplicación", fr: "Application", de: "Anwendung", zh: "应用" },
  { pt: "Preferências", en: "Preferences", es: "Preferencias", fr: "Préférences", de: "Einstellungen", zh: "偏好" },
  { pt: "Modelos", en: "Models", es: "Modelos", fr: "Modèles", de: "Modelle", zh: "模型" },
  { pt: "Agente", en: "Agent", es: "Agente", fr: "Agent", de: "Agent", zh: "智能体" },
  { pt: "Conteúdo", en: "Content", es: "Contenido", fr: "Contenu", de: "Inhalt", zh: "内容" },
  { pt: "Contexto", en: "Context", es: "Contexto", fr: "Contexte", de: "Kontext", zh: "上下文" },
  { pt: "Ferramentas", en: "Tools", es: "Herramientas", fr: "Outils", de: "Werkzeuge", zh: "工具" },
  { pt: "Geral", en: "General", es: "General", fr: "Général", de: "Allgemein", zh: "常规" },
  { pt: "Contas", en: "Accounts", es: "Cuentas", fr: "Comptes", de: "Konten", zh: "账户" },
  { pt: "Delegação", en: "Routing", es: "Delegación", fr: "Routage", de: "Delegation", zh: "路由与编排" },
  { pt: "Skills", en: "Skills", es: "Skills", fr: "Skills", de: "Skills", zh: "技能 (Skills)" },
  { pt: "MCP", en: "MCP", es: "MCP", fr: "MCP", de: "MCP", zh: "MCP 服务" },
  { pt: "Mídia", en: "Media", es: "Multimedia", fr: "Médias", de: "Medien", zh: "多媒体" },
  { pt: "Agentes", en: "Agents", es: "Agentes", fr: "Agents", de: "Agenten", zh: "智能体" },
  { pt: "Histórico", en: "History", es: "Historial", fr: "Historique", de: "Verlauf", zh: "历史记录" },
  { pt: "Uso", en: "Usage", es: "Uso", fr: "Utilisation", de: "Nutzung", zh: "用量" },
  { pt: "Tema, privacidade, dados e economia de tokens.", en: "Theme, language, privacy, local data, and token savings.", es: "Tema, idioma, privacidad, datos y ahorro de tokens.", fr: "Thème, langue, confidentialité, données et économie de tokens.", de: "Design, Sprache, Datenschutz, Daten und Token-Ersparnis.", zh: "主题、语言、隐私、本地数据与 Token 节省。" },
  { pt: "Assinaturas, chaves de API e servidores locais num lugar só; a ordem é o failover.", en: "Subscriptions, API keys, and local servers in one place; order defines failover.", es: "Suscripciones, claves API y servidores locales en un solo lugar; el orden define el failover.", fr: "Abonnements, clés API et serveurs locaux au même endroit ; l'ordre définit le basculement.", de: "Abos, API-Schlüssel und lokale Server an einem Ort; die Reihenfolge bestimmt das Failover.", zh: "集中管理订阅、API 密钥与本地服务器；按顺序自动故障转移。" },
  { pt: "Quanto de cada plano já foi usado e quando volta.", en: "How much of each plan quota is used and when it resets.", es: "Cuánto de cada plan se ha usado y cuándo se reinicia.", fr: "Quota utilisé pour chaque forfait et heure de réinitialisation.", de: "Verbrauchtes Kontingent pro Tarif und Reset-Zeitpunkt.", zh: "各订阅套餐已用配额及重置时间。" },
  { pt: "Qual modelo cuida de cada papel do agente.", en: "Which model handles each agent role and delegation profile.", es: "Qué modelo se encarga de cada rol del agente.", fr: "Quel modèle gère chaque rôle de l'agent.", de: "Welches Modell welche Agentenrolle übernimmt.", zh: "为每个智能体角色分配专用模型。" },
  { pt: "Tokens e requisições da janela atual.", en: "Tokens and requests in the current time window.", es: "Tokens y solicitudes en la ventana actual.", fr: "Tokens et requêtes de la fenêtre actuelle.", de: "Tokens und Anfragen im aktuellen Zeitfenster.", zh: "当前时间窗口内的 Token 与请求统计。" },
  { pt: "O que ele lembra de você entre conversas.", en: "What Zerith remembers about you across conversations.", es: "Lo que recuerda sobre ti entre conversaciones.", fr: "Ce qu'il retient de vous entre les discussions.", de: "Was Zerith sich über Gespräche hinweg merkt.", zh: "跨对话为您持久记住的偏好与事实。" },
  { pt: "Habilidades e plugins instalados.", en: "Installed skills, repositories, and plugins.", es: "Habilidades y plugins instalados.", fr: "Compétences et plugins installés.", de: "Installierte Skills und Plugins.", zh: "已安装的技能与插件。" },
  { pt: "Servidores MCP: ferramentas externas do agente.", en: "MCP servers: external tools for the agent.", es: "Servidores MCP: herramientas externas del agente.", fr: "Serveurs MCP : outils externes de l'agent.", de: "MCP-Server: externe Werkzeuge für den Agenten.", zh: "MCP 服务器：外部扩展工具。" },
  { pt: "Subagentes especializados.", en: "Specialized subagents.", es: "Subagentes especializados.", fr: "Sous-agents spécialisés.", de: "Spezialisierte Subagenten.", zh: "专用子智能体。" },
  { pt: "Modelos de imagem e vídeo.", en: "Image and video generation models.", es: "Modelos de imagen y vídeo.", fr: "Modèles d'image et de vidéo.", de: "Bild- und Videomodelle.", zh: "图像与视频生成模型。" },
  { pt: "Conversas salvas e importação.", en: "Saved conversations and import.", es: "Conversaciones guardadas e importación.", fr: "Discussions enregistrées et importation.", de: "Gespeicherte Chats und Import.", zh: "已保存的对话与导入。" },

  // General Tab
  { pt: "Aparência", en: "Appearance", es: "Apariencia", fr: "Apparence", de: "Darstellung", zh: "外观" },
  { pt: "Como a janela se apresenta.", en: "How the window looks and feels.", es: "Cómo se presenta la ventana.", fr: "Présentation de la fenêtre.", de: "Aussehen und Verhalten des Fensters.", zh: "窗口界面外观设置。" },
  { pt: "Idioma", en: "Language", es: "Idioma", fr: "Langue", de: "Sprache", zh: "语言" },
  { pt: "Idioma da interface do aplicativo.", en: "Display language for the desktop interface.", es: "Idioma de la interfaz de la aplicación.", fr: "Langue d'affichage de l'application.", de: "Anzeigesprache der Benutzeroberfläche.", zh: "桌面应用程序界面显示语言。" },
  { pt: "Tema", en: "Theme", es: "Tema", fr: "Thème", de: "Design", zh: "主题" },
  { pt: "Escuro", en: "Dark", es: "Oscuro", fr: "Sombre", de: "Dunkel", zh: "深色" },
  { pt: "Claro", en: "Light", es: "Claro", fr: "Clair", de: "Hell", zh: "浅色" },
  { pt: "Sistema", en: "System", es: "Sistema", fr: "Système", de: "System", zh: "跟随系统" },
  { pt: "“Sistema” acompanha o modo claro ou escuro do computador.", en: "“System” follows your OS light or dark mode.", es: "“Sistema” sigue el modo claro u oscuro del equipo.", fr: "« Système » suit le thème clair ou sombre de l'OS.", de: "„System“ folgt dem hellen oder dunklen OS-Modus.", zh: "“跟随系统”将自动匹配操作系统的深浅色模式。" },
  { pt: "Movimento reduzido", en: "Reduced motion", es: "Movimiento reducido", fr: "Mouvement réduit", de: "Reduzierte Bewegung", zh: "减少动画" },
  { pt: "Troca animações por cortes e fades curtos.", en: "Replaces animations with quick cuts and fades.", es: "Reemplaza animaciones por transiciones rápidas.", fr: "Remplace les animations par des fondus courts.", de: "Ersetzt Animationen durch kurze Übergänge.", zh: "用快速切换代替平滑动画效果。" },
  { pt: "Sobre o Zerith", en: "About Zerith", es: "Acerca de Zerith", fr: "À propos de Zerith", de: "Über Zerith", zh: "关于 Zerith" },
  { pt: "Novas versões chegam direto do GitHub.", en: "New releases arrive directly from GitHub.", es: "Las nuevas versiones llegan directo de GitHub.", fr: "Les nouvelles versions arrivent directement de GitHub.", de: "Neue Versionen kommen direkt von GitHub.", zh: "新版本直接从 GitHub 获取更新。" },
  { pt: "Atualizações", en: "Updates", es: "Actualizaciones", fr: "Mises à jour", de: "Updates", zh: "版本更新" },
  { pt: "Verificar agora", en: "Check now", es: "Verificar ahora", fr: "Vérifier maintenant", de: "Jetzt prüfen", zh: "立即检查" },
  { pt: "Verificando…", en: "Checking…", es: "Verificando…", fr: "Vérification…", de: "Wird geprüft…", zh: "正在检查…" },
  { pt: "Baixando…", en: "Downloading…", es: "Descargando…", fr: "Téléchargement…", de: "Wird heruntergeladen…", zh: "正在下载…" },
  { pt: "Atualizar", en: "Update", es: "Actualizar", fr: "Mettre à jour", de: "Aktualisieren", zh: "更新" },
  { pt: "Depois", en: "Later", es: "Después", fr: "Plus tard", de: "Später", zh: "稍后" },
  { pt: "Tentar de novo", en: "Try again", es: "Reintentar", fr: "Réessayer", de: "Erneut versuchen", zh: "重试" },
  { pt: "Privacidade", en: "Privacy", es: "Privacidad", fr: "Confidentialité", de: "Datenschutz", zh: "隐私" },
  { pt: "O que o agente pode lembrar entre conversas.", en: "What the agent can remember across chats.", es: "Lo que el agente puede recordar entre conversaciones.", fr: "Ce que l'agent peut retenir entre les discussions.", de: "Was sich der Agent zwischen Chats merken darf.", zh: "智能体在对话之间可记住的信息。" },
  { pt: "Enquanto estiver ligado, as próximas conversas não leem nem gravam lembranças.", en: "While enabled, future chats neither read nor save memories.", es: "Mientras esté activo, las conversaciones no leen ni guardan recuerdos.", fr: "Lorsqu'activé, les discussions ne lisent ni n'enregistrent de souvenirs.", de: "Wenn aktiv, lesen oder speichern zukünftige Chats keine Erinnerungen.", zh: "启用后，后续对话不会读取或保存任何记忆。" },
  { pt: "Aprender sozinho (memória e skills)", en: "Self-learning (memory & skills)", es: "Autoaprendizaje (memoria y skills)", fr: "Auto-apprentissage (mémoire et skills)", de: "Selbstlernend (Gedächtnis & Skills)", zh: "自主学习 (记忆与技能)" },
  { pt: "Aprender sozinho", en: "Self-learning", es: "Autoaprendizaje", fr: "Auto-apprentissage", de: "Selbstlernend", zh: "自主学习" },
  { pt: "revisão em segundo plano", en: "background review", es: "revisión en segundo plano", fr: "révision en arrière-plan", de: "Hintergrundprüfung", zh: "后台复盘" },
  { pt: "Depois de um turno com correção, preferência ou trabalho com várias ferramentas, uma revisão rápida guarda o que vale lembrar. Turnos simples não custam nada. Cada item aparece no chat com Desfazer.", en: "After a turn with a correction, preference, or multi-tool workflow, a quick review saves durable facts or skills. Simple turns cost nothing. Each learned item appears with an Undo button.", es: "Tras un turno con corrección, preferencia o varias herramientas, una revisión rápida guarda lo importante. Cada elemento aparece en el chat con Deshacer.", fr: "Après un tour avec correction, préférence ou plusieurs outils, une révision rapide enregistre ce qui compte. Chaque élément apparaît avec Annuler.", de: "Nach einem Schritt mit Korrektur, Präferenz oder mehreren Tools speichert eine kurze Prüfung Wichtiges. Jeder Eintrag hat einen Rückgängig-Button.", zh: "在包含纠正、偏好或多工具操作的对话后，快速复盘并保存有价值的记忆或技能，并在聊天中提供撤销按钮。" },
  { pt: "Modelo da revisão", en: "Reviewer model", es: "Modelo de revisión", fr: "Modèle de révision", de: "Prüfmodell", zh: "复盘模型" },
  { pt: "Um modelo barato basta: a revisão lê só um resumo do turno. “Mesmo da conversa” usa o modelo que respondeu.", en: "A fast, inexpensive model is enough: the review only reads a turn digest. “Same as chat” uses the active conversation model.", es: "Un modelo económico basta: la revisión solo lee un resumen del turno. “Mismo de la conversación” usa el modelo activo.", fr: "Un modèle économique suffit : la révision ne lit qu'un résumé. « Même que la discussion » utilise le modèle actif.", de: "Ein günstiges Modell reicht: die Prüfung liest nur eine Zusammenfassung. „Wie im Chat“ nutzt das Antwortmodell.", zh: "使用轻量模型即可：复盘仅读取对话摘要。“与对话相同”将使用当前回答的模型。" },
  { pt: "Mesmo da conversa", en: "Same as chat", es: "Mismo de la conversación", fr: "Même que la discussion", de: "Wie im Chat", zh: "与对话相同" },
  { pt: "Perfil de execução", en: "Execution profile", es: "Perfil de ejecución", fr: "Profil d'exécution", de: "Ausführungsprofil", zh: "执行配置" },
  { pt: "Troca velocidade por custo.", en: "Balance speed, context depth, and token cost.", es: "Equilibra velocidad y costo.", fr: "Équilibre vitesse et coût.", de: "Abwägung zwischen Geschwindigkeit und Kosten.", zh: "平衡响应速度与 Token 成本。" },
  { pt: "Perfil", en: "Profile", es: "Perfil", fr: "Profil", de: "Profil", zh: "配置" },
  { pt: "Balanceado", en: "Balanced", es: "Equilibrado", fr: "Équilibré", de: "Ausgewogen", zh: "均衡" },
  { pt: "Desempenho", en: "Performance", es: "Rendimiento", fr: "Performance", de: "Leistung", zh: "高性能" },
  { pt: "Quanto o agente lê e delega por turno.", en: "How much history the agent reads and how many subagents run in parallel.", es: "Cuánto lee y delega el agente por turno.", fr: "Combien l'agent lit et délègue par tour.", de: "Wie viel der Agent pro Schritt liest und delegiert.", zh: "每轮读取的历史消息数与并行子智能体上限。" },
  { pt: "Seus dados", en: "Your data", es: "Tus datos", fr: "Vos données", de: "Deine Daten", zh: "本地数据" },
  { pt: "Tudo fica local, no seu computador.", en: "Everything stays local on your computer.", es: "Todo queda local en tu equipo.", fr: "Tout reste local sur votre ordinateur.", de: "Alles bleibt lokal auf deinem Computer.", zh: "所有数据均保存在您的本地计算机上。" },
  { pt: "Pasta de dados", en: "Data folder", es: "Carpeta de datos", fr: "Dossier de données", de: "Datenordner", zh: "数据目录" },
  { pt: "Conversas, memória, skills e configurações ficam aqui, só no seu computador.", en: "Chats, memory, skills, and settings live here on your machine.", es: "Conversaciones, memoria, skills y ajustes viven aquí en tu equipo.", fr: "Discussions, mémoire, skills et paramètres sont stockés ici.", de: "Chats, Gedächtnis, Skills und Einstellungen liegen lokal hier.", zh: "对话、记忆、技能和配置文件均保存在此本地目录中。" },
  { pt: "Abrir", en: "Open", es: "Abrir", fr: "Ouvrir", de: "Öffnen", zh: "打开" },
  { pt: "Abrir pasta", en: "Open folder", es: "Abrir carpeta", fr: "Ouvrir le dossier", de: "Ordner öffnen", zh: "打开文件夹" },
  { pt: "Banco de conversas", en: "Conversations database", es: "Base de datos de conversaciones", fr: "Base de données des discussions", de: "Chat-Datenbank", zh: "对话数据库" },
  { pt: "Calculando…", en: "Calculating…", es: "Calculando…", fr: "Calcul…", de: "Wird berechnet…", zh: "计算中…" },
  { pt: "Backup", en: "Backup", es: "Copia de seguridad", fr: "Sauvegarde", de: "Backup", zh: "备份" },
  { pt: "Cópia consistente do banco na pasta backups/, feita com o app aberto.", en: "Consistent hot backup of the SQLite database in backups/.", es: "Copia consistente de la base de datos en backups/.", fr: "Sauvegarde cohérente de la base dans backups/.", de: "Konsistentes Datenbank-Backup im Ordner backups/.", zh: "在应用运行时创建一致的数据库备份至 backups/ 目录。" },
  { pt: "Fazer backup", en: "Create backup", es: "Hacer copia", fr: "Sauvegarder", de: "Backup erstellen", zh: "立即备份" },
  { pt: "Copiando…", en: "Backing up…", es: "Copiando…", fr: "Copie…", de: "Wird kopiert…", zh: "正在备份…" },
  { pt: "Economia de tokens", en: "Token savings", es: "Ahorro de tokens", fr: "Économie de tokens", de: "Token-Ersparnis", zh: "Token 节省" },
  { pt: "Menos tokens por turno, mesma resposta.", en: "Fewer tokens per turn, same answer quality.", es: "Menos tokens por turno, misma calidad.", fr: "Moins de tokens par tour, même qualité.", de: "Weniger Tokens pro Schritt bei gleicher Qualität.", zh: "每轮消耗更少 Token，保持同等回答质量。" },
  { pt: "Conversas longas", en: "Long conversations", es: "Conversaciones largas", fr: "Longues discussions", de: "Lange Gespräche", zh: "超长上下文" },
  { pt: "Desligado: o Zerith resume a conversa sozinho quando ela enche.", en: "Off: Zerith automatically compacts the chat when the context fills up.", es: "Apagado: Zerith resume la conversación automáticamente cuando se llena.", fr: "Désactivé : Zerith résume automatiquement la discussion lorsqu'elle est pleine.", de: "Aus: Zerith fasst das Gespräch automatisch zusammen, wenn der Kontext voll wird.", zh: "关闭：当上下文接近上限时，Zerith 会自动压缩摘要。" },
  { pt: "Não instalado neste computador.", en: "Not installed on this computer.", es: "No instalado en este equipo.", fr: "Non installé sur cet ordinateur.", de: "Auf diesem Computer nicht installiert.", zh: "此计算机尚未安装。" },
  { pt: "Iniciando…", en: "Starting…", es: "Iniciando…", fr: "Démarrage…", de: "Startet…", zh: "正在启动…" },
  { pt: "Ativo: o histórico antigo é dobrado antes de ir ao modelo.", en: "Active: older history is folded before being sent to the model.", es: "Activo: el historial antiguo se pliega antes de enviarse al modelo.", fr: "Actif : l'ancien historique est compressé avant l'envoi au modèle.", de: "Aktiv: älterer Verlauf wird vor dem Senden an das Modell gefaltet.", zh: "已启用：旧历史记录在发送给模型前会自动折叠压缩。" },
  { pt: "Não iniciou; o chat segue direto.", en: "Did not start; chat continues directly.", es: "No inició; el chat continúa directo.", fr: "N'a pas démarré ; le chat continue directement.", de: "Nicht gestartet; Chat läuft direkt weiter.", zh: "未启动；对话将直接连接模型。" },
  { pt: "Instalar", en: "Install", es: "Instalar", fr: "Installer", de: "Installieren", zh: "安装" },
  { pt: "Instalando…", en: "Installing…", es: "Instalando…", fr: "Installation…", de: "Wird installiert…", zh: "正在安装…" },
  { pt: "Desligado", en: "Off", es: "Apagado", fr: "Désactivé", de: "Aus", zh: "关闭" },
  { pt: "Leve", en: "Lite", es: "Ligero", fr: "Léger", de: "Leicht", zh: "轻度" },
  { pt: "Máximo", en: "Ultra", es: "Máximo", fr: "Ultra", de: "Maximum", zh: "极致" },
  { pt: "Código enxuto", en: "Lean code", es: "Código conciso", fr: "Code concis", de: "Schlanker Code", zh: "精简代码" },
  { pt: "Pede ao agente o mínimo que resolve: reaproveitar o que já existe, sem camadas a mais. Nível mais alto, diff menor.", en: "Instructs the agent to write the minimal effective diff and reuse existing code.", es: "Pide al agente el cambio mínimo necesario reutilizando lo existente.", fr: "Demande à l'agent le diff minimal en réutilisant l'existant.", de: "Weist den Agenten an, minimalen Code zu schreiben und Bestehendes wiederzuverwenden.", zh: "要求智能体编写最小化变更并复用现有代码。" },
  { pt: "Respostas curtas", en: "Concise replies", es: "Respuestas cortas", fr: "Réponses concises", de: "Kurze Antworten", zh: "精简回答" },
  { pt: "O agente explica em poucas palavras. Código, caminhos, comandos e avisos de segurança continuam completos.", en: "The agent explains in fewer words while keeping code, paths, and commands complete.", es: "El agente explica en pocas palabras manteniendo código, rutas y comandos completos.", fr: "L'agent explique en peu de mots tout en gardant le code et les commandes complets.", de: "Der Agent erklärt knapp, während Code, Pfade und Befehle vollständig bleiben.", zh: "智能体用言简意赅的语言回答，同时保留完整代码、路径与命令。" },
  { pt: "Resumir saídas longas", en: "Compress long tool outputs", es: "Resumir salidas largas", fr: "Compresser les sorties longues", de: "Lange Ausgaben komprimieren", zh: "压缩长命令输出" },
  { pt: "Comandos como git, build e grep podem imprimir milhares de linhas. Só o essencial vai ao modelo; o texto completo continua no terminal.", en: "Commands like git, build, grep, or BGP tables can print thousands of lines. Only the essential signal goes to the model.", es: "Comandos como git, build y grep pueden imprimir miles de líneas. Solo lo esencial va al modelo.", fr: "Les commandes comme git, build et grep peuvent afficher des milliers de lignes. Seul l'essentiel est envoyé au modèle.", de: "Befehle wie git, build und grep erzeugen oft tausende Zeilen. Nur das Wesentliche geht an das Modell.", zh: "对 git、构建、grep 或路由表等千行输出提取核心信息发送给模型，显著节省 Token。" },

  // Accounts, Quotas, Routing, Usage
  { pt: "Todos", en: "All", es: "Todos", fr: "Tous", de: "Alle", zh: "全部" },
  { pt: "Assinaturas", en: "Subscriptions", es: "Suscripciones", fr: "Abonnements", de: "Abonnements", zh: "订阅账户" },
  { pt: "Locais", en: "Local", es: "Locales", fr: "Locaux", de: "Lokal", zh: "本地服务" },
  { pt: "Chave de API", en: "API Key", es: "Clave de API", fr: "Clé API", de: "API-Schlüssel", zh: "API 密钥" },
  { pt: "Chave", en: "Key", es: "Clave", fr: "Clé", de: "Schlüssel", zh: "密钥" },
  { pt: "Conectados", en: "Connected", es: "Conectados", fr: "Connectés", de: "Verbunden", zh: "已连接" },
  { pt: "Outro provedor", en: "Custom provider", es: "Otro proveedor", fr: "Autre fournisseur", de: "Anderer Anbieter", zh: "自定义提供商" },
  { pt: "Entre pelo navegador com a conta que você já paga. Sem chave para colar.", en: "Sign in via browser with the subscription you already pay for. No API key needed.", es: "Inicia sesión en el navegador con tu suscripción actual. Sin pegar claves.", fr: "Connectez-vous via le navigateur avec votre abonnement existant. Sans clé API.", de: "Melde dich im Browser mit deinem bestehenden Abo an. Kein API-Key nötig.", zh: "通过浏览器登录您现有的订阅账户，无需粘贴 API 密钥。" },
  { pt: "Servidores rodando na sua máquina. Sem chave e sem custo.", en: "Servers running on your machine. No key and zero cost.", es: "Servidores ejecutándose en tu equipo. Sin clave y sin costo.", fr: "Serveurs tournant sur votre machine. Sans clé et sans coût.", de: "Server auf deinem eigenen Rechner. Ohne Schlüssel und kostenlos.", zh: "在本地计算机上运行的模型服务，无需密钥且零成本。" },
  { pt: "Cole a chave do provedor e escolha o modelo.", en: "Paste your provider API key and pick a default model.", es: "Pega la clave del proveedor y elige el modelo.", fr: "Collez la clé du fournisseur et choisissez le modèle.", de: "Füge den API-Schlüssel ein und wähle das Modell.", zh: "粘贴提供商 API 密钥并选择默认模型。" },
  { pt: "Assinatura Claude Pro ou Max", en: "Claude Pro or Max subscription", es: "Suscripción Claude Pro o Max", fr: "Abonnement Claude Pro ou Max", de: "Claude Pro oder Max Abo", zh: "Claude Pro 或 Max 订阅" },
  { pt: "Assinatura ChatGPT Plus ou Pro", en: "ChatGPT Plus or Pro subscription", es: "Suscripción ChatGPT Plus o Pro", fr: "Abonnement ChatGPT Plus ou Pro", de: "ChatGPT Plus oder Pro Abo", zh: "ChatGPT Plus 或 Pro 订阅" },
  { pt: "Conta Google · Gemini", en: "Google Account · Gemini", es: "Cuenta Google · Gemini", fr: "Compte Google · Gemini", de: "Google-Konto · Gemini", zh: "Google 账户 · Gemini" },
  { pt: "verificando…", en: "checking…", es: "verificando…", fr: "vérification…", de: "prüfe…", zh: "检查中…" },
  { pt: "sem credencial", en: "no credential", es: "sin credencial", fr: "sans identifiant", de: "keine Anmeldedaten", zh: "无凭证" },
  { pt: "expirada", en: "expired", es: "expirada", fr: "expiré", de: "abgelaufen", zh: "已过期" },
  { pt: "Principal", en: "Main (Brain)", es: "Principal", fr: "Principal", de: "Hauptmodell", zh: "主模型 (思考)" },
  { pt: "Conversa com você e decide o que delegar.", en: "Talks with you, plans architecture, and decides what to delegate.", es: "Conversa contigo y decide qué delegar.", fr: "Discute avec vous et décide quoi déléguer.", de: "Spricht mit dir und entscheidet, was delegiert wird.", zh: "与您对话、规划架构并决定委派哪些任务。" },
  { pt: "Executam as tarefas delegadas.", en: "Executes delegated operational tasks and tool loops.", es: "Ejecutan las tareas delegadas.", fr: "Exécutent les tâches déléguées.", de: "Führen delegierte Aufgaben aus.", zh: "执行委派的操作任务与工具调用。" },
  { pt: "Explorar", en: "Explore", es: "Explorar", fr: "Explorer", de: "Erkunden", zh: "探索 (Explore)" },
  { pt: "Lê e busca no código, sem editar.", en: "Reads and searches code without modifying files.", es: "Lee y busca en el código sin editar.", fr: "Lit et cherche dans le code sans modifier.", de: "Liest und durchsucht Code ohne Änderungen.", zh: "只读搜索与浏览代码，不修改文件。" },
  { pt: "Planejar", en: "Plan", es: "Planificar", fr: "Planifier", de: "Planen", zh: "规划 (Plan)" },
  { pt: "Monta o passo a passo no modo Plano.", en: "Builds step-by-step implementation plans in Plan mode.", es: "Arma el paso a paso en modo Plan.", fr: "Construit le plan étape par étape.", de: "Erstellt Schritt-für-Schritt-Pläne im Planmodus.", zh: "在计划模式下制定分步实施方案。" },
  { pt: "Compactar", en: "Compact", es: "Compactar", fr: "Compacter", de: "Komprimieren", zh: "压缩 (Compact)" },
  { pt: "Resume históricos longos preservando entidades.", en: "Summarizes long histories while preserving key technical entities.", es: "Resume historiales largos preservando entidades.", fr: "Résume les longs historiques en préservant les entités.", de: "Fasst lange Verläufe unter Erhalt wichtiger Entitäten zusammen.", zh: "压缩长对话历史并保留关键技术实体。" },
  { pt: "modelo da conversa", en: "conversation model", es: "modelo de la conversación", fr: "modèle de la discussion", de: "Chat-Modell", zh: "当前对话模型" },
  { pt: "o que você escolher no chat", en: "whichever model you pick in the chat bar", es: "el que elijas en el chat", fr: "celui choisi dans le chat", de: "das im Chat gewählte Modell", zh: "您在聊天栏中选择的模型" },
  { pt: "Salvar", en: "Save", es: "Guardar", fr: "Enregistrer", de: "Speichern", zh: "保存" },
  { pt: "Cancelar", en: "Cancel", es: "Cancelar", fr: "Annuler", de: "Abbrechen", zh: "取消" },
  { pt: "Testar conexão", en: "Test connection", es: "Probar conexión", fr: "Tester la connexion", de: "Verbindung testen", zh: "测试连接" },
  { pt: "Remover", en: "Remove", es: "Eliminar", fr: "Supprimer", de: "Entfernen", zh: "移除" },
  { pt: "Excluir", en: "Delete", es: "Eliminar", fr: "Supprimer", de: "Löschen", zh: "删除" },
  { pt: "Editar", en: "Edit", es: "Editar", fr: "Modifier", de: "Bearbeiten", zh: "编辑" },
  { pt: "Entrar", en: "Sign in", es: "Iniciar sesión", fr: "Se connecter", de: "Anmelden", zh: "登录" },
  { pt: "Sair", en: "Sign out", es: "Cerrar sesión", fr: "Se déconnecter", de: "Abmelden", zh: "退出登录" },
  { pt: "Copiado", en: "Copied", es: "Copiado", fr: "Copié", de: "Kopiert", zh: "已复制" },
  { pt: "Copiar link", en: "Copy link", es: "Copiar enlace", fr: "Copier le lien", de: "Link kopieren", zh: "复制链接" },

  // Skills, Plugins & MCP
  { pt: "Instaladas", en: "Installed", es: "Instaladas", fr: "Installées", de: "Installiert", zh: "已安装" },
  { pt: "Catálogo", en: "Catalog", es: "Catálogo", fr: "Catalogue", de: "Katalog", zh: "目录" },
  { pt: "Plugins", en: "Plugins", es: "Plugins", fr: "Plugins", de: "Plugins", zh: "插件" },
  { pt: "Risco alto", en: "High risk", es: "Riesgo alto", fr: "Risque élevé", de: "Hohes Risiko", zh: "高风险" },
  { pt: "Atenção", en: "Warning", es: "Atención", fr: "Attention", de: "Warnung", zh: "警告" },
  { pt: "Notas", en: "Notes", es: "Notas", fr: "Notes", de: "Hinweise", zh: "提示" },
  { pt: "Nota", en: "Note", es: "Nota", fr: "Note", de: "Hinweis", zh: "提示" },
  { pt: "Sem alertas", en: "No alerts", es: "Sin alertas", fr: "Aucune alerte", de: "Keine Warnungen", zh: "无警报" },
  { pt: "Importável", en: "Importable", es: "Importable", fr: "Importable", de: "Importierbar", zh: "可导入" },
  { pt: "Comando não encontrado", en: "Command not found", es: "Comando no encontrado", fr: "Commande introuvable", de: "Befehl nicht gefunden", zh: "未找到命令" },
  { pt: "Já existe", en: "Already exists", es: "Ya existe", fr: "Existe déjà", de: "Bereits vorhanden", zh: "已存在" },
  { pt: "Configuração inválida", en: "Invalid configuration", es: "Configuración inválida", fr: "Configuration invalide", de: "Ungültige Konfiguration", zh: "配置无效" },

  // Agents, Memory, History, Media
  { pt: "Embutido", en: "Built-in", es: "Integrado", fr: "Intégré", de: "Integriert", zh: "内置" },
  { pt: "Pessoal", en: "User", es: "Personal", fr: "Personnel", de: "Benutzer", zh: "个人" },
  { pt: "Do projeto", en: "Project", es: "Del proyecto", fr: "Du projet", de: "Projekt", zh: "项目" },
  { pt: "De plugin", en: "Plugin", es: "De plugin", fr: "De plugin", de: "Plugin", zh: "插件" },
  { pt: "Novo agente", en: "New agent", es: "Nuevo agente", fr: "Nouvel agent", de: "Neuer Agent", zh: "新建智能体" },
  { pt: "Criar agente", en: "Create agent", es: "Crear agente", fr: "Créer l'agent", de: "Agent erstellen", zh: "创建智能体" },
  { pt: "Nenhum agente", en: "No agents", es: "Ningún agente", fr: "Aucun agent", de: "Keine Agenten", zh: "暂无智能体" },
  { pt: "Sem descrição.", en: "No description.", es: "Sin descripción.", fr: "Aucune description.", de: "Keine Beschreibung.", zh: "暂无描述。" },
  { pt: "Nome", en: "Name", es: "Nombre", fr: "Nom", de: "Name", zh: "名称" },
  { pt: "Descrição", en: "Description", es: "Descripción", fr: "Description", de: "Beschreibung", zh: "描述" },
  { pt: "Instruções", en: "Instructions", es: "Instrucciones", fr: "Instructions", de: "Anweisungen", zh: "指令" },
  { pt: "Fatos guardados", en: "Saved facts", es: "Hechos guardados", fr: "Faits enregistrés", de: "Gespeicherte Fakten", zh: "已存事实" },
  { pt: "Confirmados por você", en: "Confirmed by you", es: "Confirmados por ti", fr: "Confirmés par vous", de: "Von dir bestätigt", zh: "您已确认" },
  { pt: "Inferidos pela IA", en: "Inferred by AI", es: "Inferidos por la IA", fr: "Inférés par l'IA", de: "Von der KI abgeleitet", zh: "AI 自动归纳" },
  { pt: "Gravação", en: "Recording", es: "Grabación", fr: "Enregistrement", de: "Aufzeichnung", zh: "记忆记录" },
  { pt: "Buscar na memória", en: "Search memory", es: "Buscar en la memoria", fr: "Rechercher dans la mémoire", de: "Gedächtnis durchsuchen", zh: "搜索记忆" },
  { pt: "Memória vazia", en: "Memory is empty", es: "Memoria vacía", fr: "Mémoire vide", de: "Gedächtnis ist leer", zh: "记忆为空" },
  { pt: "Fatos", en: "Facts", es: "Hechos", fr: "Faits", de: "Fakten", zh: "事实" },
  { pt: "Decisões", en: "Decisions", es: "Decisiones", fr: "Décisions", de: "Entscheidungen", zh: "决策" },
  { pt: "Projetos", en: "Projects", es: "Proyectos", fr: "Projets", de: "Projekte", zh: "项目" },
  { pt: "Entidades", en: "Entities", es: "Entidades", fr: "Entités", de: "Entitäten", zh: "实体" },
  { pt: "Ativas", en: "Active", es: "Activas", fr: "Actives", de: "Aktiv", zh: "活跃" },
  { pt: "Todas", en: "All", es: "Todas", fr: "Toutes", de: "Alle", zh: "全部" },
  { pt: "Buscar no título e nas mensagens", en: "Search titles and messages", es: "Buscar en títulos y mensajes", fr: "Rechercher dans les titres et messages", de: "In Titeln und Nachrichten suchen", zh: "搜索标题与消息内容" },
  { pt: "Nenhuma conversa", en: "No conversations", es: "Ninguna conversación", fr: "Aucune discussion", de: "Keine Gespräche", zh: "暂无对话" },
  { pt: "Arquivada", en: "Archived", es: "Archivada", fr: "Archivée", de: "Archiviert", zh: "已归档" },
  { pt: "Arquivar", en: "Archive", es: "Archivar", fr: "Archiver", de: "Archivieren", zh: "归档" },
  { pt: "Fixadas", en: "Pinned", es: "Fijadas", fr: "Épinglées", de: "Angeheftet", zh: "已置顶" },
  { pt: "Fixar", en: "Pin", es: "Fijar", fr: "Épingler", de: "Anheften", zh: "置顶" },
  { pt: "Desafixar", en: "Unpin", es: "Desfijar", fr: "Désépingler", de: "Lösen", zh: "取消置顶" },
  { pt: "Renomear", en: "Rename", es: "Renombrar", fr: "Renommer", de: "Umbenennen", zh: "重命名" },
  { pt: "Novo fato", en: "New fact", es: "Nuevo hecho", fr: "Nouveau fait", de: "Neuer Fakt", zh: "新增记忆" },
  { pt: "Importar do Hermes", en: "Import from Hermes", es: "Importar de Hermes", fr: "Importer d'Hermes", de: "Aus Hermes importieren", zh: "从 Hermes 导入" },
  { pt: "Confirmado", en: "Confirmed", es: "Confirmado", fr: "Confirmé", de: "Bestätigt", zh: "已确认" },
  { pt: "Inferido", en: "Inferred", es: "Inferido", fr: "Inféré", de: "Abgeleitet", zh: "AI 归纳" },
  { pt: "Apagar", en: "Delete", es: "Eliminar", fr: "Supprimer", de: "Löschen", zh: "删除" },
  { pt: "Restaurar", en: "Restore", es: "Restaurar", fr: "Restaurer", de: "Wiederherstellen", zh: "恢复" },
  { pt: "Exportar", en: "Export", es: "Exportar", fr: "Exporter", de: "Exportieren", zh: "导出" },
  { pt: "Imagem", en: "Image", es: "Imagen", fr: "Image", de: "Bild", zh: "图像" },
  { pt: "Vídeo", en: "Video", es: "Vídeo", fr: "Vidéo", de: "Video", zh: "视频" },
  { pt: "Cofre do sistema", en: "System keyring", es: "Bóveda del sistema", fr: "Trousseau système", de: "System-Tresor", zh: "系统密钥库" },
  { pt: "Variável de ambiente", en: "Environment variable", es: "Variable de entorno", fr: "Variable d'environnement", de: "Umgebungsvariable", zh: "环境变量" },
  { pt: "Minimizar", en: "Minimize", es: "Minimizar", fr: "Réduire", de: "Minimieren", zh: "最小化" },
  { pt: "Maximizar", en: "Maximize", es: "Maximizar", fr: "Agrandir", de: "Maximieren", zh: "最大化" },
  { pt: "Fechar", en: "Close", es: "Cerrar", fr: "Fermer", de: "Schließen", zh: "关闭" },
  { pt: "Copiar", en: "Copy", es: "Copiar", fr: "Copier", de: "Kopieren", zh: "复制" },
  { pt: "Ver código", en: "View code", es: "Ver código", fr: "Voir le code", de: "Code anzeigen", zh: "查看代码" },
  { pt: "Ver diagrama", en: "View diagram", es: "Ver diagrama", fr: "Voir le diagramme", de: "Diagramm anzeigen", zh: "查看拓扑图" },
  { pt: "Pensando…", en: "Thinking…", es: "Pensando…", fr: "Réflexion…", de: "Denkt nach…", zh: "正在思考…" },
  { pt: "Raciocínio", en: "Reasoning", es: "Razonamiento", fr: "Raisonnement", de: "Gedankengang", zh: "思考过程" },
  { pt: "A resposta não terminou", en: "Response interrupted", es: "La respuesta no terminó", fr: "La réponse s'est interrompue", de: "Antwort unterbrochen", zh: "回答未完成" },
  { pt: "Ramificar daqui", en: "Branch from here", es: "Ramificar desde aquí", fr: "Créer une branche d'ici", de: "Von hier abzweigen", zh: "从此处分支" },
  { pt: "Desfazer", en: "Undo", es: "Deshacer", fr: "Annuler", de: "Rückgängig", zh: "撤销" },
  { pt: "Abrir fora", en: "Open externally", es: "Abrir fuera", fr: "Ouvrir à l'extérieur", de: "Extern öffnen", zh: "外部打开" },
  { pt: "Carregar", en: "Load", es: "Cargar", fr: "Charger", de: "Laden", zh: "加载" },
  { pt: "Conversa vazia. Escreva abaixo para começar; use / para comandos e @ para citar arquivos.", en: "Empty conversation. Type below to start; use / for commands and @ to mention files.", es: "Conversación vacía. Escribe abajo para empezar; usa / para comandos y @ para archivos.", fr: "Discussion vide. Écrivez ci-dessous pour commencer ; utilisez / pour les commandes et @ pour les fichiers.", de: "Leerer Chat. Schreibe unten, um zu starten; nutze / für Befehle und @ für Dateien.", zh: "空白对话。在下方输入开始对话；使用 / 调用命令，使用 @ 引用文件。" },
  { pt: "Lendo os agentes…", en: "Loading agents…", es: "Leyendo agentes…", fr: "Chargement des agents…", de: "Agenten werden geladen…", zh: "正在加载智能体…" },
  { pt: "Lendo a memória…", en: "Loading memory…", es: "Leyendo memoria…", fr: "Chargement de la mémoire…", de: "Gedächtnis wird geladen…", zh: "正在加载记忆…" },
  { pt: "Lendo o histórico…", en: "Loading history…", es: "Leyendo historial…", fr: "Chargement de l'historique…", de: "Verlauf wird geladen…", zh: "正在加载历史记录…" },
  { pt: "Um agente é um arquivo .md: o nome do arquivo é o nome, o texto é a instrução.", en: "An agent is a .md file: the filename is its name and the body is its system instruction.", es: "Un agente es un archivo .md: el nombre del archivo es su nombre y el texto su instrucción.", fr: "Un agent est un fichier .md : le nom du fichier est son nom, le texte est son instruction.", de: "Ein Agent ist eine .md-Datei: der Dateiname ist der Name, der Text die Anweisung.", zh: "智能体是一个 .md 文件：文件名为名称，正文为系统指令。" },
  { pt: "Vira um arquivo .md na pasta de agentes.", en: "Saved as a .md file in your agents folder.", es: "Se guarda como un archivo .md en la carpeta de agentes.", fr: "Devient un fichier .md dans le dossier des agents.", de: "Wird als .md-Datei im Agentenordner gespeichert.", zh: "将保存为智能体目录中的 .md 文件。" },
  { pt: "Crie um em “Novo agente” ou solte um .md na pasta.", en: "Create one with “New agent” or drop a .md file in the folder.", es: "Crea uno en “Nuevo agente” o suelta un .md en la carpeta.", fr: "Créez-en un via « Nouvel agent » ou déposez un .md dans le dossier.", de: "Erstelle einen über „Neuer Agent“ oder lege eine .md-Datei im Ordner ab.", zh: "点击“新建智能体”创建，或将 .md 文件放入文件夹中。" },
  { pt: "Regras que o agente sempre segue.", en: "Rules the agent always follows.", es: "Reglas que el agente siempre sigue.", fr: "Règles que l'agent suit toujours.", de: "Regeln, die der Agent immer befolgt.", zh: "智能体始终遵循的规则。" },
  { pt: "Como você gosta que ele trabalhe.", en: "How you prefer the agent to work.", es: "Cómo prefieres que trabaje.", fr: "Comment vous préférez qu'il travaille.", de: "Wie der Agent bevorzugt arbeiten soll.", zh: "您偏好的工作方式。" },
  { pt: "Coisas que são verdade sobre você.", en: "Durable facts about you and your environment.", es: "Hechos duraderos sobre ti y tu entorno.", fr: "Faits durables sur vous et votre environnement.", de: "Fakten über dich und deine Umgebung.", zh: "关于您与您的环境的持久事实。" },
  { pt: "Escolhas já tomadas; ele não as reabre.", en: "Decisions already made that should not be reopened.", es: "Decisiones ya tomadas que no se reabren.", fr: "Décisions déjà prises.", de: "Bereits getroffene Entscheidungen.", zh: "已确定的技术与业务决策。" },
  { pt: "O que você está construindo.", en: "What you are building and maintaining.", es: "Lo que estás construyendo.", fr: "Ce que vous construisez.", de: "Was du baust und verwaltest.", zh: "您正在构建和维护的项目。" },
  { pt: "Pessoas, empresas e lugares citados.", en: "People, clients, companies, and sites mentioned.", es: "Personas, clientes, empresas y sitios citados.", fr: "Personnes, clients, entreprises et lieux cités.", de: "Erwähnte Personen, Kunden, Firmen und Standorte.", zh: "提及的人员、客户、公司与站点。" },
  { pt: "O agente aprende com as conversas e usa o que sabe nas próximas. Tudo fica neste computador.", en: "The agent learns from conversations and recalls relevant facts in future chats. Everything stays local.", es: "El agente aprende de las conversaciones y lo usa en las próximas. Todo queda en este equipo.", fr: "L'agent apprend des discussions et l'utilise dans les suivantes. Tout reste local.", de: "Der Agent lernt aus Gesprächen für zukünftige Chats. Alles bleibt lokal.", zh: "智能体从对话中学习并在后续对话中自动召回。所有数据均保存在本地。" },
  { pt: "Enquanto ligado, as conversas não leem nem gravam lembranças.", en: "While enabled, conversations neither read nor save memories.", es: "Mientras esté activo, las conversaciones no leen ni guardan recuerdos.", fr: "Lorsqu'activé, les discussions ne lisent ni n'enregistrent de souvenirs.", de: "Wenn aktiv, lesen oder speichern Chats keine Erinnerungen.", zh: "启用后，对话不会读取或写入记忆。" },
  { pt: "Diga \"lembre que …\" numa conversa para guardar um fato.", en: "Say \"remember that …\" in a chat to save a durable fact.", es: "Di \"recuerda que …\" en una conversación para guardar un hecho.", fr: "Dites « souviens-toi que … » dans une discussion pour enregistrer un fait.", de: "Sage „merke dir, dass …“ im Chat, um einen Fakt zu speichern.", zh: "在对话中说“记住…”即可保存一条持久事实。" },
  { pt: "Nenhum fato corresponde à busca.", en: "No facts match your search.", es: "Ningún hecho coincide con la búsqueda.", fr: "Aucun fait ne correspond à la recherche.", de: "Kein Fakt entspricht der Suche.", zh: "没有匹配的记忆事实。" },
  { pt: "As conversas aparecem aqui assim que você começa uma.", en: "Conversations appear here as soon as you start one.", es: "Las conversaciones aparecen aquí en cuanto empiezas una.", fr: "Les discussions apparaissent ici dès que vous en commencez une.", de: "Gespräche erscheinen hier, sobald du eines startest.", zh: "开始新对话后将在此处显示。" },
];

const BY_PT = new Map<string, Entry>();
const BY_EN = new Map<string, Entry>();
for (const e of PHRASES) {
  BY_PT.set(e.pt, e);
  BY_EN.set(e.en, e);
}

/** Looks up an exact phrase or dynamic pattern in the target language. */
export function translateText(raw: string, lang: Lang): string {
  const trimmed = raw.trim();
  if (!trimmed) return raw;

  const exact = BY_PT.get(trimmed) ?? BY_EN.get(trimmed);
  if (exact) {
    const out = exact[lang === "pt-BR" ? "pt" : lang];
    return raw.replace(trimmed, out);
  }

  if (lang === "pt-BR") return raw;

  // Dynamic pattern rules (counts, statuses, model prefixes)
  const patterns: [RegExp, (m: RegExpMatchArray) => Record<Exclude<Lang, "pt-BR">, string>][] = [
    [
      /^(\d+)\s+rodando$/,
      (m) => ({
        en: `${m[1]} running`,
        es: `${m[1]} en ejecución`,
        fr: `${m[1]} en cours`,
        de: `${m[1]} aktiv`,
        zh: `${m[1]} 运行中`,
      }),
    ],
    [
      /^(\d+)\s+no turno$/,
      (m) => ({
        en: `${m[1]} in turn`,
        es: `${m[1]} en el turno`,
        fr: `${m[1]} dans le tour`,
        de: `${m[1]} im Schritt`,
        zh: `本轮 ${m[1]} 个`,
      }),
    ],
    [
      /^Mostrar ocultos \((\d+)\)$/,
      (m) => ({
        en: `Show hidden (${m[1]})`,
        es: `Mostrar ocultos (${m[1]})`,
        fr: `Afficher les cachés (${m[1]})`,
        de: `Ausgeblendete zeigen (${m[1]})`,
        zh: `显示隐藏项 (${m[1]})`,
      }),
    ],
    [
      /^Versão atual\s+(.+)$/,
      (m) => ({
        en: `Current version ${m[1]}`,
        es: `Versión actual ${m[1]}`,
        fr: `Version actuelle ${m[1]}`,
        de: `Aktuelle Version ${m[1]}`,
        zh: `当前版本 ${m[1]}`,
      }),
    ],
    [
      /^Nova versão\s+(.+)\s+disponível$/,
      (m) => ({
        en: `New version ${m[1]} available`,
        es: `Nueva versión ${m[1]} disponible`,
        fr: `Nouvelle version ${m[1]} disponible`,
        de: `Neue Version ${m[1]} verfügbar`,
        zh: `新版本 ${m[1]} 可用`,
      }),
    ],
    [
      /^expira em\s+(.+)$/,
      (m) => ({
        en: `expires in ${m[1]}`,
        es: `expira en ${m[1]}`,
        fr: `expire dans ${m[1]}`,
        de: `läuft ab in ${m[1]}`,
        zh: `${m[1]}后过期`,
      }),
    ],
    [
      /^(\d+)\s+conversas?(.*)$/,
      (m) => ({
        en: `${m[1]} chat(s)${m[2].includes("arquivadas") ? ", including archived" : ""}`,
        es: `${m[1]} conversación(es)${m[2]}`,
        fr: `${m[1]} discussion(s)${m[2]}`,
        de: `${m[1]} Chat(s)${m[2]}`,
        zh: `${m[1]} 个对话`,
      }),
    ],
  ];

  for (const [re, fn] of patterns) {
    const match = trimmed.match(re);
    if (match) {
      const mapped = fn(match)[lang];
      return raw.replace(trimmed, mapped);
    }
  }

  return raw;
}

export function tr(
  lang: Lang,
  pt: string,
  en: string,
  extra?: Partial<Record<Lang, string>>,
): string {
  if (extra && extra[lang]) return extra[lang]!;
  if (lang === "pt-BR") return pt;
  if (lang === "en") return en;
  const hit = BY_PT.get(pt.trim()) ?? BY_EN.get(en.trim());
  if (hit) return hit[lang];
  const translated = translateText(pt, lang);
  if (translated !== pt) return translated;
  return en;
}

const ORIG_TEXT = new WeakMap<Text, string>();
const ORIG_ATTR = new WeakMap<Element, Map<string, string>>();
const SKIP_TAGS = new Set(["SCRIPT", "STYLE", "CODE", "PRE", "TEXTAREA"]);
const ATTRS = ["placeholder", "title", "aria-label"] as const;

function shouldSkipElement(el: Element | null): boolean {
  if (!el) return true;
  if (SKIP_TAGS.has(el.tagName)) return true;
  if (
    el.closest(
      "pre, code, textarea, .mono, .mono-path, .term-tail, .side-list .side-row-title",
    )
  ) {
    return true;
  }
  return false;
}

function applyDomTranslation(root: Node, lang: Lang): void {
  const walker = document.createTreeWalker(
    root,
    NodeFilter.SHOW_ELEMENT | NodeFilter.SHOW_TEXT,
  );
  let node: Node | null = walker.currentNode;
  while (node) {
    if (node.nodeType === Node.TEXT_NODE) {
      const textNode = node as Text;
      if (!shouldSkipElement(textNode.parentElement)) {
        const current = textNode.nodeValue ?? "";
        const orig = ORIG_TEXT.get(textNode);
        if (orig === undefined) {
          if (current.trim()) ORIG_TEXT.set(textNode, current);
        } else {
          // If React updated the text node to a new value, update our stored original
          const prevTranslated = translateText(orig, lang);
          if (current !== orig && current !== prevTranslated && current.trim()) {
            ORIG_TEXT.set(textNode, current);
          }
        }
        const base = ORIG_TEXT.get(textNode);
        if (base) {
          const next = translateText(base, lang);
          if (textNode.nodeValue !== next) {
            textNode.nodeValue = next;
          }
        }
      }
    } else if (node.nodeType === Node.ELEMENT_NODE) {
      const el = node as Element;
      let attrMap = ORIG_ATTR.get(el);
      for (const attr of ATTRS) {
        const val = el.getAttribute(attr);
        if (!val) continue;
        if (!attrMap) {
          attrMap = new Map<string, string>();
          ORIG_ATTR.set(el, attrMap);
        }
        const orig = attrMap.get(attr);
        if (orig === undefined) {
          attrMap.set(attr, val);
        }
        const base = attrMap.get(attr) ?? val;
        const next = translateText(base, lang);
        if (val !== next) {
          el.setAttribute(attr, next);
        }
      }
    }
    node = walker.nextNode();
  }
}

/** Reactive DOM & attribute translator that localizes every screen and modal into `lang`. */
export function useDomTranslator(lang: Lang): void {
  useEffect(() => {
    if (typeof document === "undefined" || !document.body) return;
    document.documentElement.lang = lang;
    applyDomTranslation(document.body, lang);
    let raf = 0;
    const observer = new MutationObserver(() => {
      if (raf) return;
      raf = requestAnimationFrame(() => {
        raf = 0;
        if (document.body) applyDomTranslation(document.body, lang);
      });
    });
    observer.observe(document.body, {
      childList: true,
      subtree: true,
      characterData: false,
    });
    return () => {
      if (raf) cancelAnimationFrame(raf);
      observer.disconnect();
    };
  }, [lang]);
}
