Pod::Spec.new do |s|
  s.name          = 'PuppetVoice'
  s.version       = '0.0.1'
  s.summary       = 'On-device dictation and streamed speech playback'
  s.homepage      = 'https://puppet-master.xyz'
  s.license       = 'MIT'
  s.author        = 'Puppet Master'
  s.source        = { git: '' }

  s.platform      = :ios, '17.0'
  s.swift_version = '5.0'
  s.source_files  = '*.swift'

  s.dependency 'ExpoModulesCore'
end
