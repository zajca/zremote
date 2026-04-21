use gpui::Svg;

#[derive(Debug, Clone, Copy)]
pub enum Icon {
    Plus,
    X,
    Pin,
    PinOff,
    GitBranch,
    GitBranchPlus,
    GitMerge,
    FolderGit,
    SquareTerminal,
    Server,
    Wifi,
    WifiOff,
    ChevronLeft,
    ChevronRight,
    ChevronUp,
    ChevronDown,
    Loader,
    Search,
    Command,
    Folder,
    Zap,
    MessageCircle,
    CircleHelp,
    Bot,
    AlertTriangle,
    CheckCircle,
    XCircle,
    Info,
    Settings,
    PanelRight,
    FileText,
    Clock,
    Columns,
    Rows,
}

impl Icon {
    pub fn path(self) -> &'static str {
        match self {
            Self::Plus => "icons/plus.svg",
            Self::X => "icons/x.svg",
            Self::Pin => "icons/pin.svg",
            Self::PinOff => "icons/pin-off.svg",
            Self::GitBranch => "icons/git-branch.svg",
            Self::GitBranchPlus => "icons/git-branch-plus.svg",
            Self::GitMerge => "icons/git-merge.svg",
            Self::FolderGit => "icons/folder-git-2.svg",
            Self::SquareTerminal => "icons/square-terminal.svg",
            Self::Server => "icons/server.svg",
            Self::Wifi => "icons/wifi.svg",
            Self::WifiOff => "icons/wifi-off.svg",
            Self::ChevronLeft => "icons/chevron-left.svg",
            Self::ChevronRight => "icons/chevron-right.svg",
            Self::ChevronUp => "icons/chevron-up.svg",
            Self::ChevronDown => "icons/chevron-down.svg",
            Self::Loader => "icons/loader.svg",
            Self::Search => "icons/search.svg",
            Self::Command => "icons/command.svg",
            Self::Folder => "icons/folder.svg",
            Self::Zap => "icons/zap.svg",
            Self::MessageCircle => "icons/message-circle.svg",
            Self::CircleHelp => "icons/circle-help.svg",
            Self::Bot => "icons/bot.svg",
            Self::AlertTriangle => "icons/alert-triangle.svg",
            Self::CheckCircle => "icons/check-circle.svg",
            Self::XCircle => "icons/x-circle.svg",
            Self::Info => "icons/info.svg",
            Self::Settings => "icons/settings.svg",
            Self::PanelRight => "icons/panel-right.svg",
            Self::FileText => "icons/file-text.svg",
            Self::Clock => "icons/clock.svg",
            Self::Columns => "icons/columns-2.svg",
            Self::Rows => "icons/rows-2.svg",
        }
    }
}

/// Create a sized SVG element for the given icon.
pub fn icon(icon: Icon) -> Svg {
    gpui::svg().path(icon.path())
}
