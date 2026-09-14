# Adds the AinbSpikeUITests UI test target (Actuator.swift) to the generated
# ios/ainbwirespike.xcodeproj and a shared scheme that tests it. Run after
# `expo prebuild --platform ios`; prebuild output is gitignored, so this is
# re-run on every fresh prebuild.
#
#   GEM_HOME=<cocoapods libexec> ruby ios/add-actuator-target.rb
require 'xcodeproj'

root = File.expand_path('..', __dir__)
proj_path = File.join(root, 'app/ios/ainbwirespike.xcodeproj')
project = Xcodeproj::Project.open(proj_path)
name = 'AinbSpikeUITests'
# Idempotent: drop a previous copy of the target and its group first.
project.targets.select { |t| t.name == name }.each(&:remove_from_project)
project.main_group.children.select { |g| g.display_name == name }.each(&:remove_from_project)

host = project.targets.find { |t| t.name == 'ainbwirespike' }
target = project.new_target(:ui_test_bundle, name, :ios, '15.1')
group = project.new_group(name, File.join(root, 'ios'))
target.add_file_references([group.new_file(File.join(root, 'ios/Actuator.swift'))])
target.add_dependency(host)
target.build_configurations.each do |c|
  c.build_settings['PRODUCT_NAME'] = '$(TARGET_NAME)'
  c.build_settings['TEST_TARGET_NAME'] = 'ainbwirespike'
  c.build_settings['PRODUCT_BUNDLE_IDENTIFIER'] = 'dev.ainb.spike.wire.uitests'
  c.build_settings['SWIFT_VERSION'] = '5.0'
  c.build_settings['GENERATE_INFOPLIST_FILE'] = 'YES'
  c.build_settings['CODE_SIGN_STYLE'] = 'Automatic'
end
project.save

scheme = Xcodeproj::XCScheme.new
scheme.add_build_target(target)
scheme.add_test_target(target)
scheme.save_as(proj_path, name, true)
puts "added #{name}"
