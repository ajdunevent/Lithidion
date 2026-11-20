//! The 64-bit capability field (section 7): bit `n` sits in byte `n / 8`, bit `n % 8`.

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Capability {
    AppRunState = 0,
    InfiniteLogDump = 1,
    ExtendedMusic = 2,
    TwoWayDismissal = 3,
    Localization = 4,
    AppMessage8k = 5,
    HealthInsights = 6,
    AppDictation = 7,
    SendTextApp = 8,
    NotificationFiltering = 9,
    UnreadCoreDump = 10,
    WeatherApp = 11,
    RemindersApp = 12,
    WorkoutApp = 13,
    SmoothFirmwareInstallProgress = 14,
    CustomVibePatterns = 15,
    JavascriptBytecodeVersionAppended = 16,
    ImageFetch = 17,
    NotificationImages = 18,
    FirmwareUpdateAcrossDisconnection = 21,
    BlobDbVersion = 22,
    SettingsSync = 23,
    WeatherDbV4 = 24,
    UnknownTimelineAttributes = 25,
    NotificationWindowState = 26,
    AppPermissions = 27,
    MultipleLanguagePacks = 28,
}

impl Capability {
    pub const ALL: [Capability; 27] = [
        Capability::AppRunState,
        Capability::InfiniteLogDump,
        Capability::ExtendedMusic,
        Capability::TwoWayDismissal,
        Capability::Localization,
        Capability::AppMessage8k,
        Capability::HealthInsights,
        Capability::AppDictation,
        Capability::SendTextApp,
        Capability::NotificationFiltering,
        Capability::UnreadCoreDump,
        Capability::WeatherApp,
        Capability::RemindersApp,
        Capability::WorkoutApp,
        Capability::SmoothFirmwareInstallProgress,
        Capability::CustomVibePatterns,
        Capability::JavascriptBytecodeVersionAppended,
        Capability::ImageFetch,
        Capability::NotificationImages,
        Capability::FirmwareUpdateAcrossDisconnection,
        Capability::BlobDbVersion,
        Capability::SettingsSync,
        Capability::WeatherDbV4,
        Capability::UnknownTimelineAttributes,
        Capability::NotificationWindowState,
        Capability::AppPermissions,
        Capability::MultipleLanguagePacks,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Capability::AppRunState => "App run state",
            Capability::InfiniteLogDump => "Infinite log dump",
            Capability::ExtendedMusic => "Extended music",
            Capability::TwoWayDismissal => "Two-way dismissal",
            Capability::Localization => "Localization",
            Capability::AppMessage8k => "8 kB app messages",
            Capability::HealthInsights => "Health insights",
            Capability::AppDictation => "App dictation",
            Capability::SendTextApp => "Send text app",
            Capability::NotificationFiltering => "Notification filtering",
            Capability::UnreadCoreDump => "Unread core dump",
            Capability::WeatherApp => "Weather app",
            Capability::RemindersApp => "Reminders app",
            Capability::WorkoutApp => "Workout app",
            Capability::SmoothFirmwareInstallProgress => "Smooth firmware install progress",
            Capability::CustomVibePatterns => "Custom vibration patterns",
            Capability::JavascriptBytecodeVersionAppended => "JavaScript bytecode version",
            Capability::ImageFetch => "Image fetch",
            Capability::NotificationImages => "Notification images",
            Capability::FirmwareUpdateAcrossDisconnection => "Firmware update across disconnection",
            Capability::BlobDbVersion => "BlobDB version",
            Capability::SettingsSync => "Settings sync",
            Capability::WeatherDbV4 => "Weather database v4",
            Capability::UnknownTimelineAttributes => "Unknown timeline attributes",
            Capability::NotificationWindowState => "Notification window state",
            Capability::AppPermissions => "App permissions",
            Capability::MultipleLanguagePacks => "Multiple language packs",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Capabilities(pub [u8; 8]);

impl Capabilities {
    pub fn from_list(list: &[Capability]) -> Self {
        let mut c = Capabilities::default();
        for &cap in list {
            c.insert(cap);
        }
        c
    }

    pub fn insert(&mut self, cap: Capability) {
        let bit = cap as usize;
        self.0[bit / 8] |= 1 << (bit % 8);
    }

    pub fn contains(&self, cap: Capability) -> bool {
        let bit = cap as usize;
        self.0[bit / 8] & (1 << (bit % 8)) != 0
    }

    pub fn iter(&self) -> impl Iterator<Item = Capability> + '_ {
        Capability::ALL.into_iter().filter(|c| self.contains(*c))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bit_positions() {
        let c = Capabilities::from_list(&[Capability::AppRunState, Capability::WeatherApp, Capability::NotificationWindowState]);
        assert_eq!(c.0, [0x01, 0x08, 0x00, 0x04, 0, 0, 0, 0]);
        assert_eq!(c.iter().count(), 3);
        assert!(!c.contains(Capability::ExtendedMusic));
    }
}
