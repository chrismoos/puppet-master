require 'json'

Pod::Spec.new do |s|
  s.name         = 'PushCrypto'
  s.version      = '0.0.1'
  s.summary      = 'HPKE keypair generation for sealed push notifications'
  s.homepage     = 'https://puppet-master.xyz'
  s.license      = 'MIT'
  s.author       = 'Puppet Master'
  s.source       = { git: '' }

  s.platform     = :ios, '17.0'
  s.swift_version = '5.0'

  s.source_files = 'PushCryptoModule.swift'

  s.dependency 'ExpoModulesCore'
end
