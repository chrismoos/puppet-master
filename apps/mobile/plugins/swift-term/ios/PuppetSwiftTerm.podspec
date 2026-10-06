require 'json'

Pod::Spec.new do |s|
  s.name          = 'PuppetSwiftTerm'
  s.version       = '0.0.1'
  s.summary       = 'Native SwiftTerm view for Puppet Master'
  s.homepage      = 'https://puppet-master.xyz'
  s.license       = 'MIT'
  s.author        = 'Puppet Master'
  s.source        = { git: '' }

  s.platform      = :ios, '17.0'
  s.swift_version = '5.0'
  swiftterm = 'vendor/SwiftTerm/Sources/SwiftTerm'
  s.source_files  = '*.swift', "#{swiftterm}/**/*.swift"
  s.exclude_files = "#{swiftterm}/Mac/{MacDebugView,MacExtensions,MacLocalTerminalView,MacFindBarView,MacCaretView,MacTerminalView}.swift"
  s.resources     = "#{swiftterm}/Apple/Metal/Shaders.metal"

  s.dependency 'ExpoModulesCore'
end
